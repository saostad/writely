use std::cell::RefCell;
use std::ptr::NonNull;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, AnyThread, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSEvent, NSEventMask, NSFont, NSFontAttributeName,
    NSFontWeightSemibold, NSForegroundColorAttributeName, NSImage, NSMenu, NSMenuItem,
    NSMutableParagraphStyle, NSPanel, NSParagraphStyleAttributeName, NSPopUpButton,
    NSProgressIndicator, NSProgressIndicatorStyle, NSScreen, NSScrollView, NSStatusWindowLevel,
    NSStrikethroughStyleAttributeName, NSTextField, NSTextView, NSView, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSAttributedString, NSAttributedStringKey, NSDictionary, NSInteger,
    NSMutableAttributedString, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
    NSString,
};
use tao::event_loop::EventLoopProxy;

use super::{shows_copy_button, PopupTab, TabContent};
use crate::providers::RewriteStyle;
use crate::text_access::CaretRect;
use crate::UserEvent;

/// Card width; the body wraps to this minus the side padding.
const CARD_WIDTH: f64 = 440.0;
const SIDE_PAD: f64 = 12.0;
const HEADER_HEIGHT: f64 = 28.0;
const FOOTER_HEIGHT: f64 = 40.0;
/// Width of the style pop-up button in the footer: fits "Professional" plus
/// its icon and the arrows.
const STYLE_BUTTON_WIDTH: f64 = 160.0;
const COPY_BUTTON_WIDTH: f64 = 28.0;
const BODY_MIN: f64 = 40.0;
const BODY_MAX: f64 = 340.0;
/// Height of the icon panel, used to place the popup below the icon.
const ICON_SIZE: f64 = 20.0;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

pub fn init(proxy: EventLoopProxy<UserEvent>) {
    if PROXY.set(proxy).is_err() {
        // Keep the first proxy installed for this process.
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and ReplaceTarget does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct ReplaceTarget;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for ReplaceTarget {}

    impl ReplaceTarget {
        #[unsafe(method(replaceClicked:))]
        fn replace_clicked(&self, _sender: Option<&AnyObject>) {
            // The panel never activates, so this click leaves focus in the
            // user's text field; the main thread performs the replacement.
            if let Some(proxy) = PROXY.get() {
                if let Err(error) = proxy.send_event(UserEvent::ReplaceClicked) {
                    eprintln!("Writely: could not deliver the replace event: {error}");
                }
            }
        }

        #[unsafe(method(copyClicked:))]
        fn copy_clicked(&self, _sender: Option<&AnyObject>) {
            // Like Replace, but the popup stays open; the main thread copies
            // the shown suggestion.
            if let Some(proxy) = PROXY.get() {
                if let Err(error) = proxy.send_event(UserEvent::CopyClicked) {
                    eprintln!("Writely: could not deliver the copy event: {error}");
                }
            }
        }

        #[unsafe(method(retryClicked:))]
        fn retry_clicked(&self, _sender: Option<&AnyObject>) {
            // Like Replace, but the main thread re-sends the failed request.
            if let Some(proxy) = PROXY.get() {
                if let Err(error) = proxy.send_event(UserEvent::RetryClicked) {
                    eprintln!("Writely: could not deliver the retry event: {error}");
                }
            }
        }
    }
);

impl ReplaceTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject init signature is correct.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and DismissTarget does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct DismissTarget;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for DismissTarget {}

    impl DismissTarget {
        #[unsafe(method(dismissClicked:))]
        fn dismiss_clicked(&self, _sender: Option<&AnyObject>) {
            // Exactly like typing: the main thread hides the popup, bumps
            // the generation and clears the in-flight state.
            if let Some(proxy) = PROXY.get() {
                if let Err(error) = proxy.send_event(UserEvent::CaretChanged) {
                    eprintln!("Writely: could not deliver the dismiss event: {error}");
                }
            }
        }
    }
);

impl DismissTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject init signature is correct.
        unsafe { msg_send![super(this), init] }
    }
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and TabTarget does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    struct TabTarget;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for TabTarget {}

    impl TabTarget {
        #[unsafe(method(tabClicked:))]
        fn tab_clicked(&self, _sender: Option<&AnyObject>) {
            let tab = STATE.with(|state| {
                state.borrow().as_ref().and_then(|popup| {
                    popup
                        .style_button
                        .selectedItem()
                        .and_then(|item| tab_for_tag(item.tag()))
                })
            });
            let Some(tab) = tab else {
                return;
            };
            if let Some(proxy) = PROXY.get() {
                if let Err(error) = proxy.send_event(UserEvent::TabClicked(tab)) {
                    eprintln!("Writely: could not deliver the tab event: {error}");
                }
            }
        }
    }
);

impl TabTarget {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: NSObject init signature is correct.
        unsafe { msg_send![super(this), init] }
    }
}

/// Tag for menu items that never map to a tab (separators, section headers).
const MENU_TAG_NONE: NSInteger = -1;

/// Tag 0 is `Correct`; tags 1..=5 follow `RewriteStyle::ALL`.
/// Returns `None` for header/separator tags and anything unexpected.
fn tab_for_tag(tag: NSInteger) -> Option<PopupTab> {
    if tag == 0 {
        Some(PopupTab::Correct)
    } else {
        usize::try_from(tag)
            .ok()
            .and_then(|index| RewriteStyle::ALL.get(index - 1))
            .map(|style| PopupTab::Rewrite(*style))
    }
}

fn tag_for_tab(tab: PopupTab) -> NSInteger {
    match tab {
        PopupTab::Correct => 0,
        PopupTab::Rewrite(style) => RewriteStyle::ALL
            .iter()
            .position(|candidate| *candidate == style)
            .map(|index| index as NSInteger + 1)
            .unwrap_or(0),
    }
}

/// SF Symbol shown next to each style in the pop-up menu.
fn style_symbol(style: RewriteStyle) -> &'static str {
    match style {
        RewriteStyle::Friendly => "face.smiling",
        RewriteStyle::Formal => "building.columns",
        RewriteStyle::Professional => "briefcase",
        RewriteStyle::Shorter => "text.badge.minus",
        RewriteStyle::Longer => "text.badge.plus",
    }
}

/// Loads an SF Symbol for a menu item (~14 pt, template so it tints with the
/// button). A symbol missing on this macOS version logs once — the menu is
/// built once — and the item shows without an icon.
fn menu_icon(symbol: &str, label: &str) -> Option<Retained<NSImage>> {
    let name = NSString::from_str(symbol);
    let description = NSString::from_str(label);
    match NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, Some(&description)) {
        Some(image) => {
            image.setTemplate(true);
            image.setSize(NSSize::new(14.0, 14.0));
            Some(image)
        }
        None => {
            eprintln!("Writely: the menu icon {symbol} is missing on this macOS version.");
            None
        }
    }
}

/// Loads an SF Symbol for the copy button. A symbol missing on this macOS
/// version logs and the button shows a text label instead.
fn copy_icon(symbol: &str, label: &str) -> Option<Retained<NSImage>> {
    let name = NSString::from_str(symbol);
    let description = NSString::from_str(label);
    match NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, Some(&description)) {
        Some(image) => {
            image.setTemplate(true);
            image.setSize(NSSize::new(14.0, 14.0));
            Some(image)
        }
        None => {
            eprintln!("Writely: the copy icon is unavailable on this macOS version");
            None
        }
    }
}

/// Whether the copy button is visible: exactly when Replace is. macOS has no
/// copy-only mode, so the primary button always says "Replace".
fn copy_visible(replace_visible: bool) -> bool {
    shows_copy_button(replace_visible, "Replace")
}

/// Small semibold secondary-label title for a disabled section-header item.
fn section_header_title(text: &str) -> Retained<NSAttributedString> {
    // SAFETY: NSFontWeightSemibold is a framework-provided constant.
    let weight: f64 = unsafe { NSFontWeightSemibold };
    let font = NSFont::systemFontOfSize_weight(11.0, weight);
    let color = NSColor::secondaryLabelColor();
    let font_key: &NSAttributedStringKey =
        // SAFETY: NSFontAttributeName is a framework-provided constant.
        unsafe { NSFontAttributeName };
    let color_key: &NSAttributedStringKey =
        // SAFETY: NSForegroundColorAttributeName is a framework-provided constant.
        unsafe { NSForegroundColorAttributeName };
    let keys: [&NSAttributedStringKey; 2] = [font_key, color_key];
    let objects: Vec<&AnyObject> = vec![&font, &color];
    let dict = NSDictionary::from_slices(&keys, &objects);
    // SAFETY: the string and attribute dictionary hold valid objects of the expected kinds.
    unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &NSString::from_str(text),
            Some(&dict),
        )
    }
}

/// A selectable style item for the pop-up menu: `tag` maps back to the tab.
fn style_menu_item(
    mtm: MainThreadMarker,
    title: &str,
    symbol: &str,
    tag: NSInteger,
) -> Retained<NSMenuItem> {
    // SAFETY: initWithTitle:action:keyEquivalent: is NSMenuItem's designated
    // initializer; a nil action is fine — the pop-up button's own
    // target/action fires when the selection changes.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            None,
            &NSString::from_str(""),
        )
    };
    item.setTag(tag);
    if let Some(icon) = menu_icon(symbol, title) {
        item.setImage(Some(&icon));
    }
    item
}

/// A disabled section header: never selectable, tag -1.
fn section_header_item(mtm: MainThreadMarker, title: &str) -> Retained<NSMenuItem> {
    // SAFETY: initWithTitle:action:keyEquivalent: is NSMenuItem's designated
    // initializer; no action, so the item can never fire.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(mtm),
            &NSString::from_str(title),
            None,
            &NSString::from_str(""),
        )
    };
    item.setTag(MENU_TAG_NONE);
    item.setEnabled(false);
    item.setAttributedTitle(Some(&section_header_title(title)));
    item
}

fn separator_menu_item(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let item = NSMenuItem::separatorItem(mtm);
    item.setTag(MENU_TAG_NONE);
    item.setEnabled(false);
    item
}

/// The style menu for the pop-up button: "Fix grammar", then the tone styles
/// and the length styles under disabled section headers.
fn build_style_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let menu = NSMenu::new(mtm);
    menu.setAutoenablesItems(false);
    menu.addItem(&style_menu_item(mtm, "Fix grammar", "checkmark.circle", 0));
    menu.addItem(&separator_menu_item(mtm));
    menu.addItem(&section_header_item(mtm, "Change tone"));
    for style in [
        RewriteStyle::Friendly,
        RewriteStyle::Formal,
        RewriteStyle::Professional,
    ] {
        menu.addItem(&style_menu_item(
            mtm,
            style.label(),
            style_symbol(style),
            tag_for_tab(PopupTab::Rewrite(style)),
        ));
    }
    menu.addItem(&separator_menu_item(mtm));
    menu.addItem(&section_header_item(mtm, "Change length"));
    for style in [RewriteStyle::Shorter, RewriteStyle::Longer] {
        menu.addItem(&style_menu_item(
            mtm,
            style.label(),
            style_symbol(style),
            tag_for_tab(PopupTab::Rewrite(style)),
        ));
    }
    menu
}

define_class!(
    // SAFETY: NSView has no subclassing requirements and SeparatorView does not implement Drop.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    struct SeparatorView;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for SeparatorView {}

    impl SeparatorView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            NSColor::separatorColor().setFill();
            let bounds = self.bounds();
            objc2_app_kit::NSRectFill(bounds);
        }
    }
);

impl SeparatorView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(10.0, 1.0));
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: initWithFrame: is NSView's designated initializer and mtm is the main thread.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this
    }
}

struct PopupState {
    panel: Retained<NSPanel>,
    card: Retained<NSVisualEffectView>,
    scroll: Retained<NSScrollView>,
    text: Retained<NSTextView>,
    separator: Retained<SeparatorView>,
    cancel_button: Retained<NSButton>,
    replace_button: Retained<NSButton>,
    retry_button: Retained<NSButton>,
    copy_button: Retained<NSButton>,
    spinner: Retained<NSProgressIndicator>,
    style_button: Retained<NSPopUpButton>,
    #[allow(dead_code)]
    replace_target: Retained<ReplaceTarget>,
    #[allow(dead_code)]
    dismiss_target: Retained<DismissTarget>,
    #[allow(dead_code)]
    tab_target: Retained<TabTarget>,
    visible: bool,
    /// Global keyDown monitor, installed only while the popup is visible so
    /// typing hides the popup even in apps that send no AX notifications
    /// (e.g. Warp). Removed when the popup hides: zero cost at idle.
    key_monitor: Option<Retained<AnyObject>>,
}

thread_local! {
    static STATE: RefCell<Option<PopupState>> = const { RefCell::new(None) };
}

/// Rounds the card's layer without a QuartzCore dependency: the layer is
/// messaged dynamically as an `AnyObject`.
fn round_card_corners(card: &NSVisualEffectView) {
    let card_obj: &AnyObject = card;
    // SAFETY: setWantsLayer: exists on NSView; layer/setCornerRadius:/setMasksToBounds:
    // exist on the layer object once a layer is wanted.
    unsafe {
        let _: () = msg_send![card_obj, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![card_obj, layer];
        if !layer.is_null() {
            let _: () = msg_send![layer, setCornerRadius: 10.0];
            let _: () = msg_send![layer, setMasksToBounds: true];
        }
    }
}

fn ensure_popup(mtm: MainThreadMarker) {
    STATE.with(|state| {
        if state.borrow().is_some() {
            return;
        }
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 40.0));
        // SAFETY: the content rect, style mask and backing store are valid and mtm is the main thread.
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            NSPanel::alloc(mtm),
            frame,
            NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
            NSBackingStoreType::Buffered,
            false,
        );
        panel.setBecomesKeyOnlyIfNeeded(true);
        panel.setLevel(NSStatusWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Transient,
        );
        panel.setHidesOnDeactivate(false);
        panel.setHasShadow(true);
        panel.setOpaque(false);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));

        // SAFETY: initWithFrame: is the designated initializer and mtm is the main thread.
        let card = NSVisualEffectView::initWithFrame(
            NSVisualEffectView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(CARD_WIDTH, 100.0)),
        );
        card.setMaterial(NSVisualEffectMaterial::Popover);
        card.setState(NSVisualEffectState::Active);
        round_card_corners(&card);

        let title = NSTextField::labelWithString(&NSString::from_str("Writely"), mtm);
        title.setFont(Some(&NSFont::boldSystemFontOfSize(12.0)));
        title.setTextColor(Some(&NSColor::secondaryLabelColor()));
        card.addSubview(&title);

        let dismiss_target = DismissTarget::new(mtm);
        let dismiss_object: &AnyObject = &dismiss_target;
        // SAFETY: dismiss target implements dismissClicked: and lives in STATE while the panel is alive.
        let close_button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("\u{2715}"),
                Some(dismiss_object),
                Some(sel!(dismissClicked:)),
                mtm,
            )
        };
        close_button.setBordered(false);
        close_button.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        card.addSubview(&close_button);

        // SAFETY: initWithFrame: is the designated initializer and mtm is the main thread.
        let scroll = NSScrollView::initWithFrame(
            NSScrollView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 40.0)),
        );
        scroll.setHasVerticalScroller(true);
        scroll.setHasHorizontalScroller(false);
        scroll.setAutohidesScrollers(true);
        scroll.setDrawsBackground(false);

        // SAFETY: initWithFrame: is the designated initializer and mtm is the main thread.
        let text = NSTextView::initWithFrame(
            NSTextView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 40.0)),
        );
        text.setEditable(false);
        text.setSelectable(false);
        text.setDrawsBackground(false);
        text.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        text.setTextColor(Some(&NSColor::labelColor()));
        scroll.setDocumentView(Some(&text));
        card.addSubview(&scroll);

        let separator = SeparatorView::new(mtm);
        card.addSubview(&separator);

        // SAFETY: dismiss target implements dismissClicked: and lives in STATE while the panel is alive.
        let cancel_button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Cancel"),
                Some(dismiss_object),
                Some(sel!(dismissClicked:)),
                mtm,
            )
        };
        cancel_button.setKeyEquivalent(&NSString::from_str(""));
        card.addSubview(&cancel_button);

        let replace_target = ReplaceTarget::new(mtm);
        let replace_object: &AnyObject = &replace_target;
        // SAFETY: replace target implements replaceClicked: and lives in STATE while the panel is alive.
        let replace_button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Replace"),
                Some(replace_object),
                Some(sel!(replaceClicked:)),
                mtm,
            )
        };
        replace_button.setBezelColor(Some(&NSColor::controlAccentColor()));
        replace_button.setKeyEquivalent(&NSString::from_str(""));
        card.addSubview(&replace_button);

        // Retry button for failed requests: built like Replace, hidden until
        // an error-with-retry layout shows it.
        // SAFETY: replace target implements retryClicked: and lives in STATE while the panel is alive.
        let retry_button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str("Retry"),
                Some(replace_object),
                Some(sel!(retryClicked:)),
                mtm,
            )
        };
        retry_button.setBezelColor(Some(&NSColor::controlAccentColor()));
        retry_button.setKeyEquivalent(&NSString::from_str(""));
        retry_button.setHidden(true);
        card.addSubview(&retry_button);

        // Image-only copy button left of Cancel; a text label when the SF
        // Symbol is missing on this macOS version.
        // SAFETY: replace target implements copyClicked: and lives in STATE while the panel is alive.
        let copy_button = unsafe {
            NSButton::buttonWithTitle_target_action(
                &NSString::from_str(""),
                Some(replace_object),
                Some(sel!(copyClicked:)),
                mtm,
            )
        };
        copy_button.setKeyEquivalent(&NSString::from_str(""));
        if let Some(icon) = copy_icon("doc.on.doc", "Copy suggestion") {
            copy_button.setImage(Some(&icon));
        } else {
            copy_button.setTitle(&NSString::from_str("Copy"));
        }
        copy_button.setToolTip(Some(&NSString::from_str("Copy suggestion")));
        card.addSubview(&copy_button);

        // Loading spinner for "Checking…": 14 pt, hidden until needed.
        // SAFETY: initWithFrame: is the designated initializer and mtm is the main thread.
        let spinner = NSProgressIndicator::initWithFrame(
            NSProgressIndicator::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(14.0, 14.0)),
        );
        spinner.setStyle(NSProgressIndicatorStyle::Spinning);
        spinner.setDisplayedWhenStopped(false);
        spinner.setHidden(true);
        card.addSubview(&spinner);

        // Style menu button in the footer: "Fix grammar" plus the five rewrite
        // styles under section headers. Added last so the title/close views
        // stay at subview indices 0 and 1.
        let tab_target = TabTarget::new(mtm);
        let tab_object: &AnyObject = &tab_target;
        // SAFETY: initWithFrame:pullsDown: is the designated initializer and mtm is the main thread.
        let style_button = NSPopUpButton::initWithFrame_pullsDown(
            NSPopUpButton::alloc(mtm),
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                NSSize::new(STYLE_BUTTON_WIDTH, 24.0),
            ),
            false,
        );
        style_button.setMenu(Some(&build_style_menu(mtm)));
        style_button.setBordered(true);
        // Regular control size; set explicitly (0 is NSControlSizeRegular).
        // SAFETY: setControlSize: exists on NSControl.
        unsafe {
            let _: () = msg_send![&style_button, setControlSize: 0usize];
        }
        style_button.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        // SAFETY: tab target implements tabClicked: and lives in STATE while the panel is alive.
        unsafe {
            style_button.setTarget(Some(tab_object));
            style_button.setAction(Some(sel!(tabClicked:)));
        }
        style_button.setHidden(true);
        card.addSubview(&style_button);

        panel.setContentView(Some(&card));
        *state.borrow_mut() = Some(PopupState {
            panel,
            card,
            scroll,
            text,
            separator,
            cancel_button,
            replace_button,
            retry_button,
            copy_button,
            spinner,
            style_button,
            replace_target,
            dismiss_target,
            tab_target,
            visible: false,
            key_monitor: None,
        });
    });
}

/// Installs the global keyDown monitor while the popup is visible. Any key
/// gets the same handling as `CaretChanged` while visible (which hides the
/// popup and drops the in-flight suggestion).
fn install_key_monitor() {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(popup) = state.as_mut() else {
            return;
        };
        if popup.key_monitor.is_some() {
            return;
        }
        let block = block2::RcBlock::new(|_event: NonNull<NSEvent>| {
            if crate::popup::is_visible() {
                if let Some(proxy) = PROXY.get() {
                    if let Err(error) = proxy.send_event(UserEvent::CaretChanged) {
                        eprintln!("Writely: could not deliver the key event: {error}");
                    }
                }
            }
        });
        match NSEvent::addGlobalMonitorForEventsMatchingMask_handler(NSEventMask::KeyDown, &block) {
            Some(token) => {
                popup.key_monitor = Some(token);
            }
            None => {
                eprintln!("Writely: could not watch key presses; typing may not hide the popup.");
            }
        }
    });
}

fn remove_key_monitor() {
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(popup) = state.as_mut() else {
            return;
        };
        if let Some(token) = popup.key_monitor.take() {
            // SAFETY: token is the monitor returned when it was installed.
            unsafe { NSEvent::removeMonitor(&token) };
        }
    });
}

/// Which footer the plain message lays out: no footer, Cancel + Replace, or
/// Cancel + Retry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Footer {
    None,
    Replace,
    Retry,
}

/// Lays out the card for plain `text` (messages and, until the diff lands,
/// suggestions). Returns the panel height.
fn layout_plain(text: &str, is_error: bool, footer: Footer) -> f64 {
    let panel_height = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(popup) = state.as_mut() else {
            return 0.0;
        };
        popup.text.setString(&NSString::from_str(text));
        popup.text.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        if is_error {
            popup.text.setTextColor(Some(&NSColor::systemRedColor()));
        } else {
            popup.text.setTextColor(Some(&NSColor::labelColor()));
        }
        // Loading state: a small spinning indicator before "Checking…".
        let checking = text == "Checking…" && !is_error && footer == Footer::None;
        let text_indent = if checking { 18.0 } else { 0.0 };
        let text_width = CARD_WIDTH - 2.0 * SIDE_PAD - text_indent;
        // SAFETY: the text container exists for a freshly created NSTextView.
        unsafe {
            if let Some(container) = popup.text.textContainer() {
                container.setWidthTracksTextView(false);
                container.setContainerSize(NSSize::new(text_width, 1_000_000.0));
            }
        }
        popup.text.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(text_width, 1_000_000.0),
        ));
        // SAFETY: the layout manager exists for a freshly created NSTextView.
        let mut needed: f64 = BODY_MIN;
        unsafe {
            if let (Some(layout), Some(container)) =
                (popup.text.layoutManager(), popup.text.textContainer())
            {
                layout.ensureLayoutForTextContainer(&container);
                needed = layout.usedRectForTextContainer(&container).size.height;
            }
        }
        let body_height = needed.clamp(BODY_MIN, BODY_MAX);
        let has_footer = footer != Footer::None;
        let show_replace = footer == Footer::Replace;
        let show_retry = footer == Footer::Retry;
        let footer_height = if has_footer { FOOTER_HEIGHT } else { 0.0 };
        let panel_height = HEADER_HEIGHT + body_height + footer_height;

        popup.card.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(CARD_WIDTH, panel_height),
        ));
        // Header (origin is bottom-left): title left, ✕ right.
        // Title and close button frames are set by index below via stored
        // subviews: title is subview 0, close is subview 1.
        let subviews = popup.card.subviews();
        if subviews.count() >= 2 {
            let title_view = subviews.objectAtIndex(0);
            title_view.setFrame(NSRect::new(
                NSPoint::new(SIDE_PAD, panel_height - HEADER_HEIGHT + 4.0),
                NSSize::new(300.0, 20.0),
            ));
            let close_view = subviews.objectAtIndex(1);
            close_view.setFrame(NSRect::new(
                NSPoint::new(CARD_WIDTH - 36.0, panel_height - HEADER_HEIGHT + 2.0),
                NSSize::new(24.0, 24.0),
            ));
        }
        popup.scroll.setFrame(NSRect::new(
            NSPoint::new(4.0 + text_indent, footer_height),
            NSSize::new(CARD_WIDTH - 8.0 - text_indent, body_height),
        ));
        popup.text.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(text_width, body_height.max(needed)),
        ));
        if checking {
            popup.spinner.setHidden(false);
            popup.spinner.setFrame(NSRect::new(
                NSPoint::new(SIDE_PAD, footer_height + body_height - 16.0),
                NSSize::new(14.0, 14.0),
            ));
            // SAFETY: the spinner is alive in STATE for the life of the popup.
            unsafe { popup.spinner.startAnimation(None) };
        } else {
            // SAFETY: the spinner is alive in STATE for the life of the popup.
            unsafe { popup.spinner.stopAnimation(None) };
            popup.spinner.setHidden(true);
        }
        popup.style_button.setHidden(true);
        popup.separator.setHidden(!has_footer);
        if has_footer {
            popup.separator.setFrame(NSRect::new(
                NSPoint::new(0.0, footer_height),
                NSSize::new(CARD_WIDTH, 1.0),
            ));
        }
        popup.cancel_button.setHidden(!has_footer);
        popup.replace_button.setHidden(!show_replace);
        popup.retry_button.setHidden(!show_retry);
        popup.copy_button.setHidden(!copy_visible(show_replace));
        if has_footer {
            popup.replace_button.setFrame(NSRect::new(
                NSPoint::new(CARD_WIDTH - SIDE_PAD - 90.0, 8.0),
                NSSize::new(90.0, 24.0),
            ));
            popup.retry_button.setFrame(NSRect::new(
                NSPoint::new(CARD_WIDTH - SIDE_PAD - 90.0, 8.0),
                NSSize::new(90.0, 24.0),
            ));
            popup.cancel_button.setFrame(NSRect::new(
                NSPoint::new(CARD_WIDTH - SIDE_PAD - 90.0 - 8.0 - 90.0, 8.0),
                NSSize::new(90.0, 24.0),
            ));
            popup.copy_button.setFrame(NSRect::new(
                NSPoint::new(
                    CARD_WIDTH - SIDE_PAD - 90.0 - 8.0 - 90.0 - 8.0 - COPY_BUTTON_WIDTH,
                    8.0,
                ),
                NSSize::new(COPY_BUTTON_WIDTH, 24.0),
            ));
        }
        panel_height
    });
    if footer != Footer::None {
        set_copy_feedback(false);
    }
    panel_height
}

/// Builds one inline attributed text for the suggestion: unchanged tokens in
/// `labelColor`; removed tokens in red with a strikethrough; inserted tokens
/// in green, semibold. 13 pt with 3 pt line spacing, wrapping to `width`.
fn build_diff_attributed(
    original: &str,
    suggestion: &str,
    width: f64,
) -> Retained<NSMutableAttributedString> {
    let regular = NSFont::systemFontOfSize(13.0);
    let semibold = NSFont::boldSystemFontOfSize(13.0);
    let paragraph = NSMutableParagraphStyle::new();
    paragraph.setLineSpacing(3.0);
    // SAFETY: the paragraph style object is valid for the duration of the call.
    let full: Retained<NSMutableAttributedString> = NSMutableAttributedString::initWithString(
        NSMutableAttributedString::alloc(),
        &NSString::from_str(""),
    );
    let _ = width;
    for op in crate::diff::diff_words(original, suggestion) {
        let (text, font, color, strike) = match op {
            crate::diff::DiffOp::Equal(s) => (s, regular.clone(), NSColor::labelColor(), false),
            crate::diff::DiffOp::Delete(s) => (s, regular.clone(), NSColor::systemRedColor(), true),
            crate::diff::DiffOp::Insert(s) => {
                (s, semibold.clone(), NSColor::systemGreenColor(), false)
            }
        };
        let font_key: &NSAttributedStringKey =
            // SAFETY: NSFontAttributeName is a framework-provided constant.
            unsafe { NSFontAttributeName };
        let color_key: &NSAttributedStringKey =
            // SAFETY: NSForegroundColorAttributeName is a framework-provided constant.
            unsafe { NSForegroundColorAttributeName };
        let paragraph_key: &NSAttributedStringKey =
            // SAFETY: NSParagraphStyleAttributeName is a framework-provided constant.
            unsafe { NSParagraphStyleAttributeName };
        let strike_key: &NSAttributedStringKey =
            // SAFETY: NSStrikethroughStyleAttributeName is a framework-provided constant.
            unsafe { NSStrikethroughStyleAttributeName };
        let keys3: [&NSAttributedStringKey; 3] = [font_key, color_key, paragraph_key];
        let dict: Retained<NSDictionary<NSAttributedStringKey, AnyObject>> = if strike {
            let one = NSNumber::numberWithInt(1);
            let keys4: [&NSAttributedStringKey; 4] =
                [font_key, color_key, paragraph_key, strike_key];
            let objects: Vec<&AnyObject> = vec![&font, &color, &paragraph, &one];
            NSDictionary::from_slices(&keys4, &objects)
        } else {
            let objects: Vec<&AnyObject> = vec![&font, &color, &paragraph];
            NSDictionary::from_slices(&keys3, &objects)
        };
        // SAFETY: the string and attribute dictionary hold valid objects of the expected kinds.
        let segment: Retained<NSAttributedString> = unsafe {
            NSAttributedString::initWithString_attributes(
                NSAttributedString::alloc(),
                &NSString::from_str(&text),
                Some(&dict),
            )
        };
        full.appendAttributedString(&segment);
    }
    full
}

/// Lays out the card for a tabbed diff body (today's diff view with the style
/// menu button in the footer). Cancel + Replace are visible. Returns the
/// panel height.
fn layout_tabbed_diff(body: &NSAttributedString, selected: PopupTab) -> f64 {
    let panel_height = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(popup) = state.as_mut() else {
            return 0.0;
        };
        // SAFETY: the text storage exists for a freshly created NSTextView.
        unsafe {
            if let Some(storage) = popup.text.textStorage() {
                storage.setAttributedString(body);
            }
        }
        // SAFETY: the spinner is alive in STATE for the life of the popup.
        unsafe {
            popup.spinner.stopAnimation(None);
        }
        popup.spinner.setHidden(true);
        popup.text.setEditable(false);
        popup.text.setSelectable(false);
        let text_width = CARD_WIDTH - 2.0 * SIDE_PAD;
        // SAFETY: the text container exists for a freshly created NSTextView.
        unsafe {
            if let Some(container) = popup.text.textContainer() {
                container.setWidthTracksTextView(false);
                container.setContainerSize(NSSize::new(text_width, 1_000_000.0));
            }
        }
        popup.text.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(text_width, 1_000_000.0),
        ));
        // SAFETY: the layout manager exists for a freshly created NSTextView.
        let mut needed: f64 = BODY_MIN;
        unsafe {
            if let (Some(layout), Some(container)) =
                (popup.text.layoutManager(), popup.text.textContainer())
            {
                layout.ensureLayoutForTextContainer(&container);
                needed = layout.usedRectForTextContainer(&container).size.height;
            }
        }
        let body_height = needed.clamp(BODY_MIN, BODY_MAX);
        let panel_height = HEADER_HEIGHT + body_height + FOOTER_HEIGHT;

        popup.card.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(CARD_WIDTH, panel_height),
        ));
        let subviews = popup.card.subviews();
        if subviews.count() >= 2 {
            let title_view = subviews.objectAtIndex(0);
            title_view.setFrame(NSRect::new(
                NSPoint::new(SIDE_PAD, panel_height - HEADER_HEIGHT + 4.0),
                NSSize::new(300.0, 20.0),
            ));
            let close_view = subviews.objectAtIndex(1);
            close_view.setFrame(NSRect::new(
                NSPoint::new(CARD_WIDTH - 36.0, panel_height - HEADER_HEIGHT + 2.0),
                NSSize::new(24.0, 24.0),
            ));
        }
        popup.scroll.setFrame(NSRect::new(
            NSPoint::new(4.0, FOOTER_HEIGHT),
            NSSize::new(CARD_WIDTH - 8.0, body_height),
        ));
        popup.text.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(text_width, body_height.max(needed)),
        ));
        popup.separator.setHidden(false);
        popup.separator.setFrame(NSRect::new(
            NSPoint::new(0.0, FOOTER_HEIGHT),
            NSSize::new(CARD_WIDTH, 1.0),
        ));
        popup.style_button.setHidden(false);
        popup.style_button.selectItemWithTag(tag_for_tab(selected));
        popup.style_button.setFrame(NSRect::new(
            NSPoint::new(SIDE_PAD, 8.0),
            NSSize::new(STYLE_BUTTON_WIDTH, 24.0),
        ));
        popup.cancel_button.setHidden(false);
        popup.replace_button.setHidden(false);
        popup.retry_button.setHidden(true);
        popup.copy_button.setHidden(!copy_visible(true));
        popup.replace_button.setFrame(NSRect::new(
            NSPoint::new(CARD_WIDTH - SIDE_PAD - 90.0, 8.0),
            NSSize::new(90.0, 24.0),
        ));
        popup.cancel_button.setFrame(NSRect::new(
            NSPoint::new(CARD_WIDTH - SIDE_PAD - 90.0 - 8.0 - 90.0, 8.0),
            NSSize::new(90.0, 24.0),
        ));
        popup.copy_button.setFrame(NSRect::new(
            NSPoint::new(
                CARD_WIDTH - SIDE_PAD - 90.0 - 8.0 - 90.0 - 8.0 - COPY_BUTTON_WIDTH,
                8.0,
            ),
            NSSize::new(COPY_BUTTON_WIDTH, 24.0),
        ));
        panel_height
    });
    set_copy_feedback(false);
    panel_height
}

/// Lays out the card for a tabbed plain-text body (a rewrite state: loading,
/// error, or an informational note). The footer shows the style menu button
/// and Cancel, plus Retry when `retry`. Returns the panel height.
fn layout_tabbed_message(
    text: &str,
    is_error: bool,
    loading: bool,
    retry: bool,
    selected: PopupTab,
) -> f64 {
    let panel_height = STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(popup) = state.as_mut() else {
            return 0.0;
        };
        popup.text.setString(&NSString::from_str(text));
        popup.text.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        if is_error {
            popup.text.setTextColor(Some(&NSColor::systemRedColor()));
        } else {
            popup.text.setTextColor(Some(&NSColor::labelColor()));
        }
        let text_indent = if loading { 18.0 } else { 0.0 };
        let text_width = CARD_WIDTH - 2.0 * SIDE_PAD - text_indent;
        // SAFETY: the text container exists for a freshly created NSTextView.
        unsafe {
            if let Some(container) = popup.text.textContainer() {
                container.setWidthTracksTextView(false);
                container.setContainerSize(NSSize::new(text_width, 1_000_000.0));
            }
        }
        popup.text.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(text_width, 1_000_000.0),
        ));
        // SAFETY: the layout manager exists for a freshly created NSTextView.
        let mut needed: f64 = BODY_MIN;
        unsafe {
            if let (Some(layout), Some(container)) =
                (popup.text.layoutManager(), popup.text.textContainer())
            {
                layout.ensureLayoutForTextContainer(&container);
                needed = layout.usedRectForTextContainer(&container).size.height;
            }
        }
        let body_height = needed.clamp(BODY_MIN, BODY_MAX);
        let panel_height = HEADER_HEIGHT + body_height + FOOTER_HEIGHT;

        popup.card.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(CARD_WIDTH, panel_height),
        ));
        let subviews = popup.card.subviews();
        if subviews.count() >= 2 {
            let title_view = subviews.objectAtIndex(0);
            title_view.setFrame(NSRect::new(
                NSPoint::new(SIDE_PAD, panel_height - HEADER_HEIGHT + 4.0),
                NSSize::new(300.0, 20.0),
            ));
            let close_view = subviews.objectAtIndex(1);
            close_view.setFrame(NSRect::new(
                NSPoint::new(CARD_WIDTH - 36.0, panel_height - HEADER_HEIGHT + 2.0),
                NSSize::new(24.0, 24.0),
            ));
        }
        popup.scroll.setFrame(NSRect::new(
            NSPoint::new(4.0 + text_indent, FOOTER_HEIGHT),
            NSSize::new(CARD_WIDTH - 8.0 - text_indent, body_height),
        ));
        popup.text.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(text_width, body_height.max(needed)),
        ));
        if loading {
            popup.spinner.setHidden(false);
            popup.spinner.setFrame(NSRect::new(
                NSPoint::new(SIDE_PAD, FOOTER_HEIGHT + body_height - 16.0),
                NSSize::new(14.0, 14.0),
            ));
            // SAFETY: the spinner is alive in STATE for the life of the popup.
            unsafe { popup.spinner.startAnimation(None) };
        } else {
            // SAFETY: the spinner is alive in STATE for the life of the popup.
            unsafe { popup.spinner.stopAnimation(None) };
            popup.spinner.setHidden(true);
        }
        popup.separator.setHidden(false);
        popup.separator.setFrame(NSRect::new(
            NSPoint::new(0.0, FOOTER_HEIGHT),
            NSSize::new(CARD_WIDTH, 1.0),
        ));
        popup.style_button.setHidden(false);
        popup.style_button.selectItemWithTag(tag_for_tab(selected));
        popup.style_button.setFrame(NSRect::new(
            NSPoint::new(SIDE_PAD, 8.0),
            NSSize::new(STYLE_BUTTON_WIDTH, 24.0),
        ));
        popup.cancel_button.setHidden(false);
        popup.replace_button.setHidden(true);
        popup.retry_button.setHidden(!retry);
        popup.copy_button.setHidden(!copy_visible(false));
        popup.cancel_button.setFrame(NSRect::new(
            NSPoint::new(CARD_WIDTH - SIDE_PAD - 90.0 - 8.0 - 90.0, 8.0),
            NSSize::new(90.0, 24.0),
        ));
        popup.retry_button.setFrame(NSRect::new(
            NSPoint::new(CARD_WIDTH - SIDE_PAD - 90.0, 8.0),
            NSSize::new(90.0, 24.0),
        ));
        panel_height
    });
    set_copy_feedback(false);
    panel_height
}

/// Shows the popup with the style menu button showing `selected`: a word diff
/// for `Diff` (Cancel + Replace), plain text for `Message` (Cancel only, with
/// the spinner while `loading`). Positioned like every other body: the same
/// anchor and height-driven sizing, so switching styles never moves the popup.
pub fn show_tabbed(selected: PopupTab, content: TabContent, caret: &CaretRect) {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!(
            "Writely: could not show the suggestion: Writely must update it on the main thread."
        );
        return;
    };
    ensure_popup(mtm);
    let panel_height = match content {
        TabContent::Diff {
            original,
            suggestion,
        } => {
            let text_width = CARD_WIDTH - 2.0 * SIDE_PAD;
            let body = build_diff_attributed(original, suggestion, text_width);
            layout_tabbed_diff(&body, selected)
        }
        TabContent::Message {
            text,
            is_error,
            loading,
            retry,
        } => layout_tabbed_message(text, is_error, loading, retry, selected),
    };
    show_body(panel_height, caret, mtm);
}

/// Shows the popup with `text`, 4 pt below the anchor (or above the caret
/// line when there is no room below), clamped to the visible frame of the
/// screen containing the caret. AX coords start at the top-left; AppKit at
/// the bottom-left.
pub fn show_message(text: &str, is_error: bool, show_button: bool, caret: &CaretRect) {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!(
            "Writely: could not show the suggestion: Writely must update it on the main thread."
        );
        return;
    };
    ensure_popup(mtm);
    let footer = if show_button {
        Footer::Replace
    } else {
        Footer::None
    };
    let panel_height = layout_plain(text, is_error, footer);
    show_body(panel_height, caret, mtm);
}

/// Red error text with a Cancel + Retry footer (no style menu, no copy button).
pub fn show_error_with_retry(text: &str, caret: &CaretRect) {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!(
            "Writely: could not show the suggestion: Writely must update it on the main thread."
        );
        return;
    };
    ensure_popup(mtm);
    let panel_height = layout_plain(text, true, Footer::Retry);
    show_body(panel_height, caret, mtm);
}

/// Positions the already-laid-out card (of `panel_height`) at the anchor and
/// shows it. Shared by messages and suggestions.
fn show_body(panel_height: f64, caret: &CaretRect, mtm: MainThreadMarker) {
    if panel_height <= 0.0 {
        return;
    }
    let screens = NSScreen::screens(mtm);
    if screens.count() == 0 {
        return;
    }
    let primary_height = screens.objectAtIndex(0).frame().size.height;
    STATE.with(|state| {
        let mut state = state.borrow_mut();
        let Some(popup) = state.as_mut() else {
            return;
        };
        // Screen containing the caret, in AppKit coords.
        let caret_centre = NSPoint::new(
            caret.x + caret.width / 2.0,
            primary_height - (caret.y + caret.height / 2.0),
        );
        let mut container = screens.objectAtIndex(0);
        for index in 0..screens.count() {
            let screen = screens.objectAtIndex(index);
            let frame = screen.frame();
            if caret_centre.x >= frame.origin.x
                && caret_centre.x < frame.origin.x + frame.size.width
                && caret_centre.y >= frame.origin.y
                && caret_centre.y < frame.origin.y + frame.size.height
            {
                container = screen;
                break;
            }
        }
        let visible = container.visibleFrame();
        // If the card is taller than the visible frame, it takes the frame;
        // the body scrolls past BODY_MAX.
        let panel_height = panel_height.min(visible.size.height);
        let clamp_x = |x: f64| {
            x.clamp(
                visible.origin.x,
                (visible.origin.x + visible.size.width - CARD_WIDTH).max(visible.origin.x),
            )
        };
        // 4 pt below the icon; the icon sits 4 pt right of the caret.
        let icon_top = caret.y + caret.height / 2.0 - ICON_SIZE / 2.0;
        let below_top = icon_top + ICON_SIZE + 4.0;
        let below_y = primary_height - below_top - panel_height;
        let x = clamp_x(caret.x + caret.width + 4.0);
        let y = if below_y >= visible.origin.y {
            below_y.clamp(
                visible.origin.y,
                (visible.origin.y + visible.size.height - panel_height).max(visible.origin.y),
            )
        } else {
            // No room below: 4 pt above the caret line.
            let above_top = caret.y - 4.0 - panel_height;
            (primary_height - above_top - panel_height).clamp(
                visible.origin.y,
                (visible.origin.y + visible.size.height - panel_height).max(visible.origin.y),
            )
        };
        popup.panel.setFrame_display(
            NSRect::new(NSPoint::new(x, y), NSSize::new(CARD_WIDTH, panel_height)),
            true,
        );
        // The panel is created once and reused; it is never re-created.
        // NonactivatingPanel never takes focus from the user's text field.
        popup.panel.orderFront(None);
        popup.visible = true;
        drop(state);
        install_key_monitor();
    });
}

/// Swaps the copy button between the copy icon and the checkmark, updating
/// its tooltip too. Without the icon it keeps text labels so the button stays
/// readable.
pub fn set_copy_feedback(copied: bool) {
    STATE.with(|state| {
        let popup = state.borrow();
        let Some(popup) = popup.as_ref() else {
            return;
        };
        let (symbol, tip) = if copied {
            ("checkmark", "Copied")
        } else {
            ("doc.on.doc", "Copy suggestion")
        };
        if let Some(icon) = copy_icon(symbol, tip) {
            popup.copy_button.setTitle(&NSString::from_str(""));
            popup.copy_button.setImage(Some(&icon));
        } else if copied {
            popup.copy_button.setTitle(&NSString::from_str("Copied"));
            popup.copy_button.setImage(None);
        } else {
            popup.copy_button.setTitle(&NSString::from_str("Copy"));
            popup.copy_button.setImage(None);
        }
        popup.copy_button.setToolTip(Some(&NSString::from_str(tip)));
    });
}

pub fn hide() {
    remove_key_monitor();
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            // SAFETY: the spinner is alive in STATE for the life of the popup.
            unsafe {
                popup.spinner.stopAnimation(None);
            }
            popup.spinner.setHidden(true);
            popup.panel.orderOut(None);
            popup.replace_button.setHidden(true);
            popup.retry_button.setHidden(true);
            popup.cancel_button.setHidden(true);
            popup.copy_button.setHidden(true);
            popup.separator.setHidden(true);
            popup.visible = false;
        }
    });
}

pub fn is_visible() -> bool {
    STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|popup| popup.visible)
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::{tab_for_tag, tag_for_tab, NSInteger, PopupTab, RewriteStyle, MENU_TAG_NONE};

    #[test]
    fn style_tags_round_trip() {
        assert_eq!(tag_for_tab(PopupTab::Correct), 0);
        for (index, style) in RewriteStyle::ALL.iter().enumerate() {
            assert_eq!(
                tag_for_tab(PopupTab::Rewrite(*style)),
                index as NSInteger + 1
            );
        }
        for tab in [PopupTab::Correct]
            .into_iter()
            .chain(RewriteStyle::ALL.map(PopupTab::Rewrite))
        {
            assert_eq!(tab_for_tag(tag_for_tab(tab)), Some(tab));
        }
    }

    #[test]
    fn unknown_tags_map_to_no_tab() {
        assert_eq!(tab_for_tag(MENU_TAG_NONE), None);
        assert_eq!(tab_for_tag(99), None);
    }
}
