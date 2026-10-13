use std::cell::{OnceCell, RefCell};
use std::ffi::c_uchar;
use std::sync::OnceLock;

use objc2::rc::Retained;
use objc2::{define_class, msg_send, AnyThread, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSBackingStoreType, NSBezierPath, NSBitmapFormat, NSBitmapImageRep, NSCalibratedRGBColorSpace,
    NSColor, NSEvent, NSImage, NSPanel, NSScreen, NSStatusWindowLevel, NSView,
    NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString};
use tao::event_loop::EventLoopProxy;

use crate::text_access::CaretRect;
use crate::UserEvent;

const ICON_SIZE: f64 = 20.0;
const GLYPH_SIZE: f64 = 14.0;
const CORNER_RADIUS: f64 = 5.0;

static PROXY: OnceLock<EventLoopProxy<UserEvent>> = OnceLock::new();

pub fn init(proxy: EventLoopProxy<UserEvent>) {
    if PROXY.set(proxy).is_err() {
        // Keep the first proxy installed for this process.
    }
}

#[derive(Default)]
struct ClickIvars {
    image: OnceCell<Retained<NSImage>>,
}

define_class!(
    // SAFETY: NSView has no subclassing requirements and ClickView does not implement Drop.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ClickIvars]
    struct ClickView;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for ClickView {}

    impl ClickView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty_rect: NSRect) {
            let bounds = self.bounds();
            let path =
                NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bounds, CORNER_RADIUS, CORNER_RADIUS);
            NSColor::colorWithWhite_alpha(0.15, 0.92).setFill();
            path.fill();
            if let Some(image) = self.ivars().image.get() {
                let origin = NSPoint::new(
                    (bounds.size.width - GLYPH_SIZE) / 2.0,
                    (bounds.size.height - GLYPH_SIZE) / 2.0,
                );
                image.drawInRect(NSRect::new(origin, NSSize::new(GLYPH_SIZE, GLYPH_SIZE)));
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            // Plain NSView never becomes first responder, so this click neither
            // activates Writely nor takes focus from the user's text field.
            if let Some(proxy) = PROXY.get() {
                if let Err(error) = proxy.send_event(UserEvent::IconClicked) {
                    eprintln!("Writely: could not deliver the icon click event: {error}");
                }
            }
        }
    }
);

impl ClickView {
    fn new(mtm: MainThreadMarker, image: Option<Retained<NSImage>>) -> Retained<Self> {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(ICON_SIZE, ICON_SIZE));
        let this = Self::alloc(mtm).set_ivars(ClickIvars::default());
        // SAFETY: initWithFrame: is NSView's designated initializer and mtm is the main thread.
        let this: Retained<Self> = unsafe { msg_send![super(this), initWithFrame: frame] };
        if let Some(image) = image {
            let _ = this.ivars().image.set(image);
        }
        this.setWantsLayer(true);
        this.setToolTip(Some(&NSString::from_str("Check grammar")));
        this
    }
}

struct OverlayState {
    panel: Retained<NSPanel>,
    last_origin: Option<(f64, f64)>,
}

thread_local! {
    static STATE: RefCell<Option<OverlayState>> = const { RefCell::new(None) };
}

/// Builds the 14x14 "W" image once from the bundled 32x32 RGBA bytes.
fn build_icon_image() -> Option<Retained<NSImage>> {
    let bytes: &'static [u8] = include_bytes!("../../icons/tray-32.rgba");
    if bytes.len() != 32 * 32 * 4 {
        eprintln!("Writely: the bundled icon has an unexpected size.");
        return None;
    }
    // The bitmap rep references this memory without copying it, so it must
    // live forever; leaking 4 KB once for the life of the app is fine.
    let leaked: &'static mut [u8] = Box::leak(bytes.to_vec().into_boxed_slice());
    let mut plane: *mut c_uchar = leaked.as_mut_ptr().cast::<c_uchar>();
    let planes: *mut *mut c_uchar = &mut plane;
    // SAFETY: planes points at one 32x32 RGBA plane that lives for the life of
    // the app, matching the width/height/format/row-byte counts below.
    let rep = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            planes,
            32,
            32,
            8,
            4,
            true,
            false,
            NSCalibratedRGBColorSpace,
            NSBitmapFormat::AlphaNonpremultiplied,
            32 * 4,
            32,
        )
    }?;
    let image = NSImage::initWithSize(NSImage::alloc(), NSSize::new(GLYPH_SIZE, GLYPH_SIZE));
    image.addRepresentation(&rep);
    Some(image)
}

fn ensure_overlay(mtm: MainThreadMarker) {
    STATE.with(|state| {
        if state.borrow().is_some() {
            return;
        }
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(ICON_SIZE, ICON_SIZE));
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
        let view = ClickView::new(mtm, build_icon_image());
        panel.setContentView(Some(&view));
        *state.borrow_mut() = Some(OverlayState {
            panel,
            last_origin: None,
        });
    });
}

pub fn show_at(rect: &CaretRect) {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!(
            "Writely: could not show the icon: Writely must update the icon on the main thread."
        );
        return;
    };
    if crate::settings::is_settings_key_window() {
        hide();
        return;
    }
    ensure_overlay(mtm);
    let screens = NSScreen::screens(mtm);
    if screens.count() == 0 {
        hide();
        return;
    }
    let primary_height = screens.objectAtIndex(0).frame().size.height;
    // 4 pt right of the caret, vertically centred on the caret line.
    // AX coords start at the top-left; AppKit coords start at the bottom-left.
    let desired_x = rect.x + rect.width + 4.0;
    let desired_top = rect.y + rect.height / 2.0 - ICON_SIZE / 2.0;
    let caret_centre = NSPoint::new(
        rect.x + rect.width / 2.0,
        primary_height - (rect.y + rect.height / 2.0),
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
    let min_x = visible.origin.x;
    let max_x = (visible.origin.x + visible.size.width - ICON_SIZE).max(min_x);
    let min_y = visible.origin.y;
    let max_y = (visible.origin.y + visible.size.height - ICON_SIZE).max(min_y);
    let origin = (
        desired_x.clamp(min_x, max_x),
        (primary_height - desired_top - ICON_SIZE).clamp(min_y, max_y),
    );
    STATE.with(|state| {
        if let Some(overlay) = state.borrow_mut().as_mut() {
            if overlay.panel.isVisible() && overlay.last_origin == Some(origin) {
                return;
            }
            overlay.panel.setFrame_display(
                NSRect::new(
                    NSPoint::new(origin.0, origin.1),
                    NSSize::new(ICON_SIZE, ICON_SIZE),
                ),
                true,
            );
            overlay.panel.orderFront(None);
            overlay.last_origin = Some(origin);
        }
    });
}

pub fn hide() {
    STATE.with(|state| {
        if let Some(overlay) = state.borrow_mut().as_mut() {
            // The panel is kept alive and reused; it is never re-created.
            overlay.panel.orderOut(None);
            overlay.last_origin = None;
        }
    });
}
