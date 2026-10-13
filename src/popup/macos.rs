use std::cell::RefCell;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, sel, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSButton, NSColor, NSFont, NSPanel, NSScreen,
    NSStatusWindowLevel, NSTextField, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};
use tao::event_loop::EventLoopProxy;

use crate::text_access::CaretRect;
use crate::UserEvent;

/// Max width of the suggestion text; the panel is this plus padding.
const MAX_TEXT_WIDTH: f64 = 360.0;
const PADDING: f64 = 10.0;
const BUTTON_WIDTH: f64 = 90.0;
const BUTTON_HEIGHT: f64 = 24.0;
const GAP: f64 = 8.0;
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
    // SAFETY: NSView has no subclassing requirements and PopupBackgroundView does not implement Drop.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    struct PopupBackgroundView;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for PopupBackgroundView {}

    impl PopupBackgroundView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let bounds = self.bounds();
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                bounds, 8.0, 8.0,
            );
            NSColor::colorWithWhite_alpha(0.15, 0.92).setFill();
            path.fill();
        }
    }
);

impl PopupBackgroundView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 40.0));
        let this = Self::alloc(mtm).set_ivars(());
        // SAFETY: initWithFrame: is NSView's designated initializer and mtm is the main thread.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        this
    }
}

struct PopupState {
    panel: Retained<NSPanel>,
    background: Retained<PopupBackgroundView>,
    label: Retained<NSTextField>,
    button: Retained<NSButton>,
    #[allow(dead_code)]
    target: Retained<ReplaceTarget>,
    visible: bool,
}

thread_local! {
    static STATE: RefCell<Option<PopupState>> = const { RefCell::new(None) };
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

        let background = PopupBackgroundView::new(mtm);
        let label = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
        label.setTextColor(Some(&NSColor::whiteColor()));
        label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        background.addSubview(&label);

        let target = ReplaceTarget::new(mtm);
        let target_object: &AnyObject = &target;
        // SAFETY: target implements replaceClicked: and lives in STATE while the panel is alive.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Replace"),
                Some(target_object),
                Some(sel!(replaceClicked:)),
                mtm,
            )
        };
        button.setHidden(true);
        background.addSubview(&button);

        panel.setContentView(Some(&background));
        *state.borrow_mut() = Some(PopupState {
            panel,
            background,
            label,
            button,
            target,
            visible: false,
        });
    });
}

/// Shows the popup with `text`, 4 pt below the icon (or above the caret line
/// when there is no room below), clamped to the visible frame of the screen
/// containing the caret. AX coords start at the top-left; AppKit at the
/// bottom-left.
pub fn show_message(text: &str, is_error: bool, show_button: bool, caret: &CaretRect) {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!(
            "Writely: could not show the suggestion: Writely must update it on the main thread."
        );
        return;
    };
    ensure_popup(mtm);
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
        popup.label.setStringValue(&NSString::from_str(text));
        if is_error {
            popup.label.setTextColor(Some(&NSColor::systemRedColor()));
        } else {
            popup.label.setTextColor(Some(&NSColor::whiteColor()));
        }
        // Constrain the width first so the wrapping label grows vertically.
        popup.label.setFrame(NSRect::new(
            NSPoint::new(PADDING, PADDING),
            NSSize::new(MAX_TEXT_WIDTH, 10_000.0),
        ));
        popup.label.sizeToFit();
        let fitted = popup.label.frame().size;
        let text_width = fitted.width.clamp(40.0, MAX_TEXT_WIDTH);
        let text_height = fitted.height.max(17.0);

        let panel_width = MAX_TEXT_WIDTH + 2.0 * PADDING;
        let mut panel_height = PADDING + text_height + PADDING;
        if show_button {
            panel_height += GAP + BUTTON_HEIGHT;
        }
        popup.label.setFrame(NSRect::new(
            NSPoint::new(PADDING, panel_height - PADDING - text_height),
            NSSize::new(text_width, text_height),
        ));
        popup.button.setHidden(!show_button);
        if show_button {
            popup.button.setFrame(NSRect::new(
                NSPoint::new(panel_width - PADDING - BUTTON_WIDTH, PADDING),
                NSSize::new(BUTTON_WIDTH, BUTTON_HEIGHT),
            ));
        }
        popup.background.setFrame(NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(panel_width, panel_height),
        ));

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
        let clamp_x = |x: f64| {
            x.clamp(
                visible.origin.x,
                (visible.origin.x + visible.size.width - panel_width).max(visible.origin.x),
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
            NSRect::new(NSPoint::new(x, y), NSSize::new(panel_width, panel_height)),
            true,
        );
        // The panel is created once and reused; it is never re-created.
        // NonactivatingPanel never takes focus from the user's text field.
        popup.panel.orderFront(None);
        popup.visible = true;
    });
}

pub fn hide() {
    STATE.with(|state| {
        if let Some(popup) = state.borrow_mut().as_mut() {
            popup.panel.orderOut(None);
            popup.button.setHidden(true);
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
