use std::cell::{OnceCell, RefCell};
use std::ptr::NonNull;
use std::sync::Arc;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAlert, NSAlertFirstButtonReturn, NSApplication, NSBackingStoreType, NSButton, NSColor,
    NSMenu, NSMenuItem, NSPopUpButton, NSSecureTextField, NSTextField, NSWindow, NSWindowDelegate,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSRunLoop, NSSize, NSString, NSTimer, NSURL,
};

use crate::config::{AppSettings, Effort, LocalModelId, Provider};
use crate::local::{catalog, download, install, server};

const PROVIDER_TITLES: [&str; 4] = [
    "Google Gemini",
    "Together AI",
    "Ollama (local, no key needed)",
    "On this computer",
];

const EFFORT_TITLES: [&str; 5] = ["Default", "Minimal (fastest)", "Low", "Medium", "High"];

fn provider_at(index: isize) -> Provider {
    match index {
        1 => Provider::Together,
        2 => Provider::Ollama,
        3 => Provider::Local,
        _ => Provider::Gemini,
    }
}

fn provider_index(provider: Provider) -> isize {
    match provider {
        Provider::Gemini => 0,
        Provider::Together => 1,
        Provider::Ollama => 2,
        Provider::Local => 3,
    }
}

fn effort_at(index: isize) -> Effort {
    match index {
        1 => Effort::Minimal,
        2 => Effort::Low,
        3 => Effort::Medium,
        4 => Effort::High,
        _ => Effort::Default,
    }
}

fn local_model_at(index: isize) -> LocalModelId {
    match index {
        1 => LocalModelId::Qwen35_2B,
        _ => LocalModelId::Qwen35_0_8B,
    }
}

fn local_model_index(model: LocalModelId) -> isize {
    match model {
        LocalModelId::Qwen35_0_8B => 0,
        LocalModelId::Qwen35_2B => 1,
    }
}

// The five Advanced rows, in LocalFields order (threads, longest reply,
// context, startup limit, reply limit). Same strings as Linux/Windows.
const ADVANCED_LABELS: [&str; 5] = [
    "Processor threads",
    "Longest reply (tokens)",
    "Context size (tokens)",
    "Startup time limit (seconds)",
    "Reply time limit (seconds)",
];

const ADVANCED_HELPS: [&str; 5] = [
    "How many processor cores the model uses. 4 suits most computers.",
    "Stops a reply that runs too long. A token is about 4 characters.",
    "How much text the model reads at once. Larger uses more memory.",
    "How long to wait for the model to load.",
    "How long to wait for a reply.",
];

fn effort_index(effort: Effort) -> isize {
    match effort {
        Effort::Default => 0,
        Effort::Minimal => 1,
        Effort::Low => 2,
        Effort::Medium => 3,
        Effort::High => 4,
    }
}

fn model_placeholder_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => "gemini-3.5-flash-lite",
        Provider::Together => "deepseek-ai/DeepSeek-V4.1-Flash",
        Provider::Ollama => "llama3.2",
        Provider::Local => "",
    }
}

fn key_title_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => "Google Gemini API key",
        Provider::Together => "Together AI API key",
        Provider::Ollama => "Ollama address",
        Provider::Local => "Ollama address",
    }
}

fn key_help_for(provider: Provider) -> &'static str {
    match provider {
        Provider::Gemini => "Writely sends the sentence you're editing to Google Gemini to check its grammar. Get a free key at ",
        Provider::Together => "Writely sends the sentence you're editing to Together AI to check its grammar. Get a key at ",
        Provider::Ollama => "Where Ollama runs on your computer. No key needed.",
        Provider::Local => "This runs a small AI model on your computer. Download and memory options are being added.",
    }
}

#[derive(Default)]
struct DelegateIvars {
    field: OnceCell<Retained<NSSecureTextField>>,
    status: OnceCell<Retained<NSTextField>>,
}

fn set_status(status: &NSTextField, text: &str, is_error: bool) {
    status.setStringValue(&NSString::from_str(text));
    if is_error {
        status.setTextColor(Some(&NSColor::systemRedColor()));
    } else {
        status.setTextColor(Some(&NSColor::labelColor()));
    }
}

// One control's rectangle in top-down coordinates (`top` measured from the
// top of the content view), converted to AppKit's bottom-up frames when
// applied. Hidden rows keep a placeholder rectangle but take no space.
#[derive(Clone, Copy)]
struct RowRect {
    x: f64,
    top: f64,
    width: f64,
    height: f64,
}

struct Layout {
    provider_label: RowRect,
    provider_popup: RowRect,
    provider_help: RowRect,
    model_label: RowRect,
    model_field: RowRect,
    model_help: RowRect,
    effort_label: RowRect,
    effort_popup: RowRect,
    effort_help: RowRect,
    key_label: RowRect,
    field: RowRect,
    key_help: RowRect,
    link_button: RowRect,
    local_model_label: RowRect,
    local_model_popup: RowRect,
    local_model_help: RowRect,
    local_status: RowRect,
    local_download_button: RowRect,
    local_cancel_button: RowRect,
    local_delete_button: RowRect,
    local_runtime_help: RowRect,
    local_warm_checkbox: RowRect,
    local_warm_help: RowRect,
    local_idle_label: RowRect,
    local_idle_field: RowRect,
    local_idle_help: RowRect,
    local_advanced_checkbox: RowRect,
    adv_labels: [RowRect; 5],
    adv_fields: [RowRect; 5],
    adv_helps: [RowRect; 5],
    autostart_checkbox: RowRect,
    autostart_help: RowRect,
    clipboard_checkbox: RowRect,
    clipboard_help: RowRect,
    save_button: RowRect,
    remove_button: RowRect,
    hotkey_hint: RowRect,
    accessibility_button: RowRect,
    status: RowRect,
    height: f64,
}

fn compute_layout(
    is_local: bool,
    show_advanced: bool,
    has_accessibility: bool,
    status_height: f64,
) -> Layout {
    let mut cursor = 16.0;
    let mut row = |height: f64, gap: f64| -> RowRect {
        let rect = RowRect {
            x: 20.0,
            top: cursor,
            width: 380.0,
            height,
        };
        cursor += height + gap;
        rect
    };
    let provider_label = row(18.0, 4.0);
    let provider_popup = row(26.0, 2.0);
    let provider_help = row(16.0, 10.0);
    // The shared model rows are hidden for Local; they take no space then.
    let (model_label, model_field, model_help) = if is_local {
        (provider_help, provider_help, provider_help)
    } else {
        (row(18.0, 4.0), row(24.0, 2.0), row(44.0, 10.0))
    };
    let effort_label = row(18.0, 4.0);
    let effort_popup = row(26.0, 2.0);
    let effort_help = row(34.0, 10.0);
    let placeholder = effort_help;
    let (
        key_label,
        field,
        key_help,
        link_button,
        local_model_label,
        local_model_popup,
        local_model_help,
        local_status,
        local_download_button,
        local_cancel_button,
        local_delete_button,
        local_runtime_help,
        local_warm_checkbox,
        local_warm_help,
        local_idle_label,
        local_idle_field,
        local_idle_help,
        local_advanced_checkbox,
        adv_labels,
        adv_fields,
        adv_helps,
    ) = if is_local {
        let local_model_label = row(18.0, 4.0);
        let local_model_popup = row(26.0, 2.0);
        let local_model_help = row(30.0, 8.0);
        let local_status = row(36.0, 4.0);
        let buttons = row(26.0, 6.0);
        let local_download_button = RowRect {
            x: 20.0,
            width: 90.0,
            ..buttons
        };
        let local_cancel_button = RowRect {
            x: 120.0,
            width: 80.0,
            ..buttons
        };
        let local_delete_button = RowRect {
            x: 210.0,
            width: 110.0,
            ..buttons
        };
        let local_runtime_help = row(40.0, 8.0);
        let local_warm_checkbox = row(22.0, 2.0);
        let local_warm_help = row(30.0, 8.0);
        let local_idle_label = row(18.0, 4.0);
        let local_idle_field = row(24.0, 2.0);
        let local_idle_help = row(30.0, 8.0);
        let local_advanced_checkbox = row(22.0, 8.0);
        let (adv_labels, adv_fields, adv_helps) = if show_advanced {
            let help_heights = [30.0, 30.0, 30.0, 16.0, 16.0];
            let mut labels = [placeholder; 5];
            let mut fields = [placeholder; 5];
            let mut helps = [placeholder; 5];
            for (i, help_height) in help_heights.into_iter().enumerate() {
                // Label and field share a row to keep the window short.
                let adv_row = row(24.0, 2.0);
                labels[i] = RowRect {
                    x: 20.0,
                    top: adv_row.top + 3.0,
                    width: 200.0,
                    height: 18.0,
                };
                fields[i] = RowRect {
                    x: 230.0,
                    width: 170.0,
                    ..adv_row
                };
                helps[i] = row(help_height, 8.0);
            }
            (labels, fields, helps)
        } else {
            ([placeholder; 5], [placeholder; 5], [placeholder; 5])
        };
        (
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            local_model_label,
            local_model_popup,
            local_model_help,
            local_status,
            local_download_button,
            local_cancel_button,
            local_delete_button,
            local_runtime_help,
            local_warm_checkbox,
            local_warm_help,
            local_idle_label,
            local_idle_field,
            local_idle_help,
            local_advanced_checkbox,
            adv_labels,
            adv_fields,
            adv_helps,
        )
    } else {
        // The key rows and the Ollama-address rows share the same rects.
        let key_label = row(18.0, 4.0);
        let field = row(24.0, 2.0);
        let key_help = row(34.0, 2.0);
        let link_button = row(18.0, 10.0);
        (
            key_label,
            field,
            key_help,
            link_button,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            placeholder,
            [placeholder; 5],
            [placeholder; 5],
            [placeholder; 5],
        )
    };
    let autostart_checkbox = row(22.0, 2.0);
    let autostart_help = row(16.0, 10.0);
    let clipboard_checkbox = row(22.0, 2.0);
    let clipboard_help = row(34.0, 10.0);
    let buttons = row(26.0, 12.0);
    let save_button = RowRect {
        x: 20.0,
        width: 80.0,
        ..buttons
    };
    let remove_button = RowRect {
        x: 110.0,
        width: 110.0,
        ..buttons
    };
    let hotkey_hint = row(18.0, 10.0);
    let accessibility_button = if has_accessibility {
        row(24.0, 8.0)
    } else {
        placeholder
    };
    let status = row(status_height, 14.0);
    Layout {
        provider_label,
        provider_popup,
        provider_help,
        model_label,
        model_field,
        model_help,
        effort_label,
        effort_popup,
        effort_help,
        key_label,
        field,
        key_help,
        link_button,
        local_model_label,
        local_model_popup,
        local_model_help,
        local_status,
        local_download_button,
        local_cancel_button,
        local_delete_button,
        local_runtime_help,
        local_warm_checkbox,
        local_warm_help,
        local_idle_label,
        local_idle_field,
        local_idle_help,
        local_advanced_checkbox,
        adv_labels,
        adv_fields,
        adv_helps,
        autostart_checkbox,
        autostart_help,
        clipboard_checkbox,
        clipboard_help,
        save_button,
        remove_button,
        hotkey_hint,
        accessibility_button,
        status,
        height: cursor,
    }
}

fn frame_for(rect: RowRect, content_height: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(rect.x, content_height - rect.top - rect.height),
        NSSize::new(rect.width, rect.height),
    )
}

/// Re-renders the local-model status, buttons and model-popup sensitivity
/// from the current install phase for the selected model. Runs on the main
/// thread only (button actions and the repeating timer).
fn render_local() {
    struct Rows {
        selected: LocalModelId,
        runtime_error: bool,
        local_status: Retained<NSTextField>,
        download_button: Retained<NSButton>,
        cancel_button: Retained<NSButton>,
        delete_button: Retained<NSButton>,
        model_popup: Retained<NSPopUpButton>,
    }
    let rows = STATE.with(|state| {
        state.borrow().as_ref().map(|window_state| Rows {
            selected: local_model_at(window_state.local_model_popup.indexOfSelectedItem()),
            runtime_error: window_state.local_runtime_error,
            local_status: window_state.local_status.clone(),
            download_button: window_state.local_download_button.clone(),
            cancel_button: window_state.local_cancel_button.clone(),
            delete_button: window_state.local_delete_button.clone(),
            model_popup: window_state.local_model_popup.clone(),
        })
    });
    let Some(rows) = rows else { return };
    let phase = install::install_phase();
    let ready = install::model_ready(rows.selected);
    let view = super::local_view(&phase, ready, catalog::model_file(rows.selected).label);
    let busy = matches!(
        phase,
        install::InstallPhase::Downloading { .. }
            | install::InstallPhase::Verifying { .. }
            | install::InstallPhase::Unpacking
    );
    set_status(&rows.local_status, &view.status, view.is_error);
    rows.download_button
        .setHidden(!(view.show_download && !rows.runtime_error));
    rows.cancel_button
        .setHidden(!(view.show_cancel && !rows.runtime_error));
    rows.delete_button
        .setHidden(!(view.show_delete && !rows.runtime_error));
    rows.model_popup.setEnabled(view.fields_enabled);
    let finished = STATE.with(|state| {
        state
            .borrow_mut()
            .as_mut()
            .map(|window_state| {
                let finished = window_state.local_was_busy
                    && matches!(phase, install::InstallPhase::Idle)
                    && ready;
                window_state.local_was_busy = busy;
                finished
            })
            .unwrap_or(false)
    });
    if finished {
        super::notify_changed();
    }
}

/// Shows or hides the five Advanced rows from the checkbox and provider.
fn refresh_advanced_visibility() {
    let rows = STATE.with(|state| {
        state.borrow().as_ref().map(|window_state| {
            (
                provider_at(window_state.provider_popup.indexOfSelectedItem()) == Provider::Local
                    && window_state.local_advanced_checkbox.state() == 1,
                window_state.adv_labels.clone(),
                window_state.adv_fields.clone(),
                window_state.adv_helps.clone(),
            )
        })
    });
    let Some((show, labels, fields, helps)) = rows else {
        return;
    };
    for i in 0..5 {
        labels[i].setHidden(!show);
        fields[i].setHidden(!show);
        helps[i].setHidden(!show);
    }
}

/// Positions every control for what is visible now, resizes the window to
/// fit, and keeps the whole window on screen.
fn layout_window() {
    struct Rows {
        window: Retained<NSWindow>,
        provider_label: Retained<NSTextField>,
        provider_popup: Retained<NSPopUpButton>,
        provider_help: Retained<NSTextField>,
        model_label: Retained<NSTextField>,
        model_field: Retained<NSTextField>,
        model_help: Retained<NSTextField>,
        effort_label: Retained<NSTextField>,
        effort_popup: Retained<NSPopUpButton>,
        effort_help: Retained<NSTextField>,
        key_label: Retained<NSTextField>,
        field: Retained<NSSecureTextField>,
        key_help: Retained<NSTextField>,
        link_button: Retained<NSButton>,
        url_label: Retained<NSTextField>,
        url_field: Retained<NSTextField>,
        url_help: Retained<NSTextField>,
        local_model_label: Retained<NSTextField>,
        local_model_popup: Retained<NSPopUpButton>,
        local_model_help: Retained<NSTextField>,
        local_status: Retained<NSTextField>,
        local_download_button: Retained<NSButton>,
        local_cancel_button: Retained<NSButton>,
        local_delete_button: Retained<NSButton>,
        local_runtime_help: Retained<NSTextField>,
        local_warm_checkbox: Retained<NSButton>,
        local_warm_help: Retained<NSTextField>,
        local_idle_label: Retained<NSTextField>,
        local_idle_field: Retained<NSTextField>,
        local_idle_help: Retained<NSTextField>,
        local_advanced_checkbox: Retained<NSButton>,
        adv_labels: [Retained<NSTextField>; 5],
        adv_fields: [Retained<NSTextField>; 5],
        adv_helps: [Retained<NSTextField>; 5],
        autostart_checkbox: Retained<NSButton>,
        autostart_help: Retained<NSTextField>,
        clipboard_checkbox: Retained<NSButton>,
        clipboard_help: Retained<NSTextField>,
        save_button: Retained<NSButton>,
        remove_button: Retained<NSButton>,
        hotkey_hint: Retained<NSTextField>,
        accessibility_button: Option<Retained<NSButton>>,
        status: Retained<NSTextField>,
        is_local: bool,
        show_advanced: bool,
        status_height: f64,
    }
    let rows = STATE.with(|state| {
        state.borrow().as_ref().map(|ws| Rows {
            window: ws.window.clone(),
            provider_label: ws.provider_label.clone(),
            provider_popup: ws.provider_popup.clone(),
            provider_help: ws.provider_help.clone(),
            model_label: ws.model_label.clone(),
            model_field: ws.model_field.clone(),
            model_help: ws.model_help.clone(),
            effort_label: ws.effort_label.clone(),
            effort_popup: ws.effort_popup.clone(),
            effort_help: ws.effort_help.clone(),
            key_label: ws.key_label.clone(),
            field: ws.field.clone(),
            key_help: ws.key_help.clone(),
            link_button: ws.link_button.clone(),
            url_label: ws.url_label.clone(),
            url_field: ws.url_field.clone(),
            url_help: ws.url_help.clone(),
            local_model_label: ws.local_model_label.clone(),
            local_model_popup: ws.local_model_popup.clone(),
            local_model_help: ws.local_model_help.clone(),
            local_status: ws.local_status.clone(),
            local_download_button: ws.local_download_button.clone(),
            local_cancel_button: ws.local_cancel_button.clone(),
            local_delete_button: ws.local_delete_button.clone(),
            local_runtime_help: ws.local_runtime_help.clone(),
            local_warm_checkbox: ws.local_warm_checkbox.clone(),
            local_warm_help: ws.local_warm_help.clone(),
            local_idle_label: ws.local_idle_label.clone(),
            local_idle_field: ws.local_idle_field.clone(),
            local_idle_help: ws.local_idle_help.clone(),
            local_advanced_checkbox: ws.local_advanced_checkbox.clone(),
            adv_labels: ws.adv_labels.clone(),
            adv_fields: ws.adv_fields.clone(),
            adv_helps: ws.adv_helps.clone(),
            autostart_checkbox: ws.autostart_checkbox.clone(),
            autostart_help: ws.autostart_help.clone(),
            clipboard_checkbox: ws.clipboard_checkbox.clone(),
            clipboard_help: ws.clipboard_help.clone(),
            save_button: ws.save_button.clone(),
            remove_button: ws.remove_button.clone(),
            hotkey_hint: ws.hotkey_hint.clone(),
            accessibility_button: ws.accessibility_button.clone(),
            status: ws.status.clone(),
            is_local: provider_at(ws.provider_popup.indexOfSelectedItem()) == Provider::Local,
            show_advanced: ws.local_advanced_checkbox.state() == 1,
            status_height: ws.status_height,
        })
    });
    let Some(rows) = rows else { return };
    let lay = compute_layout(
        rows.is_local,
        rows.show_advanced,
        rows.accessibility_button.is_some(),
        rows.status_height,
    );
    // Clamp the content height to the screen's visible area (less the
    // title bar), then keep the resized window inside that area.
    let visible = rows.window.screen().map(|screen| screen.visibleFrame());
    let mut content_height = lay.height;
    if let Some(visible) = visible {
        let title_bar = rows.window.frame().size.height
            - rows
                .window
                .contentView()
                .map(|view| view.frame().size.height)
                .unwrap_or(0.0);
        content_height = content_height.min((visible.size.height - title_bar).max(200.0));
    }
    rows.window
        .setContentSize(NSSize::new(420.0, content_height));
    if let Some(visible) = visible {
        let mut frame = rows.window.frame();
        if frame.origin.y + frame.size.height > visible.origin.y + visible.size.height {
            frame.origin.y = visible.origin.y + visible.size.height - frame.size.height;
        }
        if frame.origin.y < visible.origin.y {
            frame.origin.y = visible.origin.y;
        }
        if frame.origin.x + frame.size.width > visible.origin.x + visible.size.width {
            frame.origin.x = visible.origin.x + visible.size.width - frame.size.width;
        }
        if frame.origin.x < visible.origin.x {
            frame.origin.x = visible.origin.x;
        }
        rows.window.setFrame_display(frame, true);
    }
    let h = content_height;
    rows.provider_label
        .setFrame(frame_for(lay.provider_label, h));
    rows.provider_popup
        .setFrame(frame_for(lay.provider_popup, h));
    rows.provider_help.setFrame(frame_for(lay.provider_help, h));
    rows.model_label.setFrame(frame_for(lay.model_label, h));
    rows.model_field.setFrame(frame_for(lay.model_field, h));
    rows.model_help.setFrame(frame_for(lay.model_help, h));
    rows.effort_label.setFrame(frame_for(lay.effort_label, h));
    rows.effort_popup.setFrame(frame_for(lay.effort_popup, h));
    rows.effort_help.setFrame(frame_for(lay.effort_help, h));
    rows.key_label.setFrame(frame_for(lay.key_label, h));
    rows.field.setFrame(frame_for(lay.field, h));
    rows.key_help.setFrame(frame_for(lay.key_help, h));
    rows.link_button.setFrame(frame_for(lay.link_button, h));
    // The Ollama-address rows share the key rows' rectangles.
    rows.url_label.setFrame(frame_for(lay.key_label, h));
    rows.url_field.setFrame(frame_for(lay.field, h));
    rows.url_help.setFrame(frame_for(lay.key_help, h));
    rows.local_model_label
        .setFrame(frame_for(lay.local_model_label, h));
    rows.local_model_popup
        .setFrame(frame_for(lay.local_model_popup, h));
    rows.local_model_help
        .setFrame(frame_for(lay.local_model_help, h));
    rows.local_status.setFrame(frame_for(lay.local_status, h));
    rows.local_download_button
        .setFrame(frame_for(lay.local_download_button, h));
    rows.local_cancel_button
        .setFrame(frame_for(lay.local_cancel_button, h));
    rows.local_delete_button
        .setFrame(frame_for(lay.local_delete_button, h));
    rows.local_runtime_help
        .setFrame(frame_for(lay.local_runtime_help, h));
    rows.local_warm_checkbox
        .setFrame(frame_for(lay.local_warm_checkbox, h));
    rows.local_warm_help
        .setFrame(frame_for(lay.local_warm_help, h));
    rows.local_idle_label
        .setFrame(frame_for(lay.local_idle_label, h));
    rows.local_idle_field
        .setFrame(frame_for(lay.local_idle_field, h));
    rows.local_idle_help
        .setFrame(frame_for(lay.local_idle_help, h));
    rows.local_advanced_checkbox
        .setFrame(frame_for(lay.local_advanced_checkbox, h));
    for i in 0..5 {
        rows.adv_labels[i].setFrame(frame_for(lay.adv_labels[i], h));
        rows.adv_fields[i].setFrame(frame_for(lay.adv_fields[i], h));
        rows.adv_helps[i].setFrame(frame_for(lay.adv_helps[i], h));
    }
    rows.autostart_checkbox
        .setFrame(frame_for(lay.autostart_checkbox, h));
    rows.autostart_help
        .setFrame(frame_for(lay.autostart_help, h));
    rows.clipboard_checkbox
        .setFrame(frame_for(lay.clipboard_checkbox, h));
    rows.clipboard_help
        .setFrame(frame_for(lay.clipboard_help, h));
    rows.save_button.setFrame(frame_for(lay.save_button, h));
    rows.remove_button.setFrame(frame_for(lay.remove_button, h));
    rows.hotkey_hint.setFrame(frame_for(lay.hotkey_hint, h));
    if let Some(button) = rows.accessibility_button.as_ref() {
        button.setFrame(frame_for(lay.accessibility_button, h));
    }
    rows.status.setFrame(frame_for(lay.status, h));
}

/// Refreshes the provider-dependent rows (model text, placeholders, key vs.
/// Ollama-address visibility, link title) for the currently selected provider.
fn refresh_provider_rows() {
    struct Rows {
        provider: Provider,
        model_label: Retained<NSTextField>,
        model_field: Retained<NSTextField>,
        model_help: Retained<NSTextField>,
        local_controls: Vec<Retained<NSTextField>>,
        local_buttons: Vec<Retained<NSButton>>,
        local_model_popup: Retained<NSPopUpButton>,
        key_field: Retained<NSSecureTextField>,
        url_field: Retained<NSTextField>,
        key_label: Retained<NSTextField>,
        key_help: Retained<NSTextField>,
        link_button: Retained<NSButton>,
        url_label: Retained<NSTextField>,
        url_help: Retained<NSTextField>,
        status: Retained<NSTextField>,
        model: String,
        ollama_url: String,
    }
    let rows = STATE.with(|state| {
        state.borrow().as_ref().map(|window_state| Rows {
            provider: provider_at(window_state.provider_popup.indexOfSelectedItem()),
            model_label: window_state.model_label.clone(),
            model_field: window_state.model_field.clone(),
            model_help: window_state.model_help.clone(),
            local_controls: vec![
                window_state.local_model_label.clone(),
                window_state.local_model_help.clone(),
                window_state.local_status.clone(),
                window_state.local_runtime_help.clone(),
                window_state.local_warm_help.clone(),
                window_state.local_idle_label.clone(),
                window_state.local_idle_field.clone(),
                window_state.local_idle_help.clone(),
            ],
            local_buttons: vec![
                window_state.local_download_button.clone(),
                window_state.local_cancel_button.clone(),
                window_state.local_delete_button.clone(),
                window_state.local_warm_checkbox.clone(),
                window_state.local_advanced_checkbox.clone(),
            ],
            local_model_popup: window_state.local_model_popup.clone(),
            key_field: window_state.field.clone(),
            url_field: window_state.url_field.clone(),
            key_label: window_state.key_label.clone(),
            key_help: window_state.key_help.clone(),
            link_button: window_state.link_button.clone(),
            url_label: window_state.url_label.clone(),
            url_help: window_state.url_help.clone(),
            status: window_state.status.clone(),
            model: window_state.settings.model_for(provider_at(
                window_state.provider_popup.indexOfSelectedItem(),
            )),
            ollama_url: window_state.settings.ollama_url.clone(),
        })
    });
    let Some(rows) = rows else { return };
    rows.model_field
        .setStringValue(&NSString::from_str(&rows.model));
    rows.model_field
        .setPlaceholderString(Some(&NSString::from_str(model_placeholder_for(
            rows.provider,
        ))));
    let is_ollama = rows.provider == Provider::Ollama;
    let is_local = rows.provider == Provider::Local;
    rows.model_label.setHidden(is_local);
    rows.model_field.setHidden(is_local);
    rows.model_help.setHidden(is_local);
    for control in &rows.local_controls {
        control.setHidden(!is_local);
    }
    for button in &rows.local_buttons {
        button.setHidden(!is_local);
    }
    rows.local_model_popup.setHidden(!is_local);
    rows.key_label.setHidden(is_ollama || is_local);
    rows.key_field.setHidden(is_ollama || is_local);
    rows.key_help.setHidden(is_ollama || is_local);
    rows.link_button.setHidden(is_ollama || is_local);
    rows.url_label.setHidden(!is_ollama);
    rows.url_field.setHidden(!is_ollama);
    rows.url_help.setHidden(!is_ollama);
    rows.key_label
        .setStringValue(&NSString::from_str(key_title_for(rows.provider)));
    rows.key_help
        .setStringValue(&NSString::from_str(key_help_for(rows.provider)));
    if let Some(page) = super::api_key_page_for(rows.provider) {
        rows.link_button.setTitle(&NSString::from_str(page));
    }
    if rows.provider.needs_key() {
        match super::has_api_key_for(rows.provider) {
            Ok(true) => rows
                .key_field
                .setPlaceholderString(Some(&NSString::from_str("••••••••"))),
            Ok(false) => rows
                .key_field
                .setPlaceholderString(Some(&NSString::from_str("Paste your key here"))),
            Err(error) => set_status(&rows.status, &error, true),
        }
    }
    if rows.url_field.stringValue().to_string().is_empty() {
        rows.url_field
            .setStringValue(&NSString::from_str(&rows.ollama_url));
    }
    rows.url_field
        .setPlaceholderString(Some(&NSString::from_str(crate::config::DEFAULT_OLLAMA_URL)));
    refresh_advanced_visibility();
    if is_local {
        // Button visibility and the model popup's enabled state follow
        // the install phase, not just the provider.
        render_local();
    }
    layout_window();
}

/// Reads the draft provider, model, effort, Ollama URL, start-at-login and
/// clipboard-fallback checkboxes from the controls.
fn read_draft() -> Option<(Provider, String, Effort, String, bool, bool)> {
    STATE.with(|state| {
        state.borrow().as_ref().map(|window_state| {
            (
                provider_at(window_state.provider_popup.indexOfSelectedItem()),
                window_state.model_field.stringValue().to_string(),
                effort_at(window_state.effort_popup.indexOfSelectedItem()),
                window_state.url_field.stringValue().to_string(),
                window_state.autostart_checkbox.state() == 1,
                window_state.clipboard_checkbox.state() == 1,
            )
        })
    })
}

/// Stores the current control values into the draft settings.
fn stash_draft() {
    if let Some((provider, model, effort, url, launch_at_login, clipboard_fallback)) = read_draft()
    {
        STATE.with(|state| {
            if let Some(window_state) = state.borrow_mut().as_mut() {
                // The model field still shows the draft provider's model: on
                // a provider change the popup already has the new selection.
                let shown = window_state.settings.provider;
                window_state.settings.set_model(shown, model.trim());
                window_state.settings.provider = provider;
                window_state.settings.effort = effort;
                window_state.settings.launch_at_login = launch_at_login;
                window_state.settings.clipboard_fallback = clipboard_fallback;
                if !url.trim().is_empty() {
                    window_state.settings.ollama_url = url.trim().to_string();
                }
            }
        });
    }
}

/// Applies a provider change: files the shown key text, stashes the draft,
/// selects `new_provider`, and shows its draft. The popup action and a tray
/// switch both call this (a programmatic `selectItemAtIndex` emits no action).
fn apply_provider_change(new_provider: Provider) {
    // File the key box text under the shown provider first (before stash_draft
    // moves settings.provider), so a typed key stays with its provider.
    let field = STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|window_state| (window_state.field.clone(), window_state.settings.provider))
    });
    if let Some((field, shown)) = field {
        let text = field.stringValue().to_string();
        STATE.with(|state| {
            if let Some(window_state) = state.borrow_mut().as_mut() {
                super::file_key_draft(&mut window_state.draft_keys, shown, &text);
            }
        });
    }
    stash_draft();
    STATE.with(|state| {
        if let Some(window_state) = state.borrow().as_ref() {
            window_state
                .provider_popup
                .selectItemAtIndex(provider_index(new_provider));
        }
    });
    STATE.with(|state| {
        if let Some(window_state) = state.borrow_mut().as_mut() {
            window_state.settings.provider = new_provider;
        }
    });
    let restore = STATE.with(|state| {
        state.borrow().as_ref().map(|window_state| {
            (
                window_state.field.clone(),
                super::restore_key_draft(&window_state.draft_keys, new_provider).to_string(),
            )
        })
    });
    if let Some((field, draft)) = restore {
        field.setStringValue(&NSString::from_str(&draft));
    }
    refresh_provider_rows();
}

/// Follows a tray switch in an open Settings window: the same steps as a
/// manual dropdown change. Returns straight away when Settings is not open.
pub fn provider_switched(provider: Provider) {
    let is_open = STATE.with(|state| state.borrow().is_some());
    if !is_open {
        return;
    }
    apply_provider_change(provider);
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements and SettingsDelegate does not implement Drop.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateIvars]
    struct SettingsDelegate;

    // SAFETY: NSObjectProtocol has no safety requirements.
    unsafe impl NSObjectProtocol for SettingsDelegate {}

    impl SettingsDelegate {
        #[unsafe(method(saveClicked:))]
        fn save_clicked(&self, _sender: Option<&AnyObject>) {
            let Some(field) = self.ivars().field.get() else {
                return;
            };
            let Some(status) = self.ivars().status.get() else {
                return;
            };
            let Some((provider, model, _, _, launch_at_login, _)) = read_draft() else {
                return;
            };
            if provider != Provider::Local {
                if let Err(error) = crate::config::validate_model_name(&model) {
                    set_status(status, &error, true);
                    return;
                }
            }
            // The local controls are read for every provider, so their
            // values are kept (and validated) even when another provider
            // is selected.
            let local_snapshot = STATE.with(|state| {
                state.borrow().as_ref().map(|window_state| {
                    (
                        local_model_at(window_state.local_model_popup.indexOfSelectedItem()),
                        window_state.local_warm_checkbox.state() == 1,
                        super::LocalFields {
                            idle_minutes: window_state
                                .local_idle_field
                                .stringValue()
                                .to_string(),
                            threads: window_state.adv_fields[0].stringValue().to_string(),
                            max_reply: window_state.adv_fields[1].stringValue().to_string(),
                            context: window_state.adv_fields[2].stringValue().to_string(),
                            startup_secs: window_state.adv_fields[3].stringValue().to_string(),
                            reply_secs: window_state.adv_fields[4].stringValue().to_string(),
                        },
                    )
                })
            });
            let Some((local_model, warm_up, local_fields)) = local_snapshot else {
                return;
            };
            let apply_result = STATE.with(|state| {
                state.borrow_mut().as_mut().map(|window_state| {
                    super::apply_local_fields(
                        &mut window_state.settings,
                        local_model,
                        warm_up,
                        &local_fields,
                    )
                })
            });
            match apply_result {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    set_status(status, &error, true);
                    return;
                }
                None => return,
            }
            stash_draft();
            let save_result = STATE.with(|state| {
                state.borrow().as_ref().map(|window_state| {
                    crate::config::save_settings(&window_state.settings)
                })
            });
            match save_result {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    set_status(status, &error, true);
                    return;
                }
                None => return,
            }
            // settings.json is written: tell the tray once, even if a key save
            // fails below.
            super::notify_changed();
            if let Err(error) = crate::autostart::set_enabled(launch_at_login) {
                set_status(status, &error, true);
                return;
            }
            super::set_autostart_message(None);
            // File the box text under the current provider, then save every
            // non-empty draft under its own provider. On the first failure the
            // remaining drafts are kept, so nothing is lost.
            let value = field.stringValue().to_string();
            STATE.with(|state| {
                if let Some(window_state) = state.borrow_mut().as_mut() {
                    super::file_key_draft(&mut window_state.draft_keys, provider, &value);
                }
            });
            let pending = STATE.with(|state| {
                state
                    .borrow()
                    .as_ref()
                    .map(|window_state| super::pending_key_drafts(&window_state.draft_keys))
                    .unwrap_or_default()
            });
            for (draft_provider, draft_key) in &pending {
                if let Err(error) = super::save_api_key_for(*draft_provider, draft_key) {
                    set_status(
                        status,
                        &format!(
                            "Could not save the {} key: {error}",
                            draft_provider.display_name()
                        ),
                        true,
                    );
                    return;
                }
                STATE.with(|state| {
                    if let Some(window_state) = state.borrow_mut().as_mut() {
                        super::clear_key_draft(&mut window_state.draft_keys, *draft_provider);
                    }
                });
            }
            set_status(status, "Saved.", false);
            field.setStringValue(&NSString::from_str(""));
            match super::has_api_key_for(provider) {
                Ok(true) => field.setPlaceholderString(Some(&NSString::from_str("••••••••"))),
                Ok(false) => {
                    field.setPlaceholderString(Some(&NSString::from_str("Paste your key here")))
                }
                Err(error) => set_status(status, &error, true),
            }
        }

        #[unsafe(method(removeClicked:))]
        fn remove_clicked(&self, _sender: Option<&AnyObject>) {
            let Some(field) = self.ivars().field.get() else {
                return;
            };
            let Some(status) = self.ivars().status.get() else {
                return;
            };
            let provider = read_draft().map(|draft| draft.0).unwrap_or(Provider::Gemini);
            match super::remove_api_key_for(provider) {
                Ok(()) => {
                    STATE.with(|state| {
                        if let Some(window_state) = state.borrow_mut().as_mut() {
                            super::clear_key_draft(&mut window_state.draft_keys, provider);
                        }
                    });
                    field.setStringValue(&NSString::from_str(""));
                    field.setPlaceholderString(Some(&NSString::from_str("Paste your key here")));
                    set_status(status, "Key removed.", false);
                    super::notify_changed();
                }
                Err(error) => set_status(status, &error, true),
            }
        }

        #[unsafe(method(openLink:))]
        fn open_link(&self, _sender: Option<&AnyObject>) {
            let Some(status) = self.ivars().status.get() else {
                return;
            };
            let provider = read_draft().map(|draft| draft.0).unwrap_or(Provider::Gemini);
            let Some(page) = super::api_key_page_for(provider) else {
                set_status(status, "Ollama runs on your computer and needs no API key.", true);
                return;
            };
            let url_string = NSString::from_str(page);
            let Some(url) = NSURL::URLWithString(&url_string) else {
                set_status(
                    status,
                    "Could not open the API key page in your browser: invalid API key page URL",
                    true,
                );
                return;
            };
            if !NSWorkspace::sharedWorkspace().openURL(&url) {
                set_status(
                    status,
                    "Could not open the API key page in your browser: NSWorkspace could not open the API key page",
                    true,
                );
            }
        }

        #[unsafe(method(providerChanged:))]
        fn provider_changed(&self, _sender: Option<&AnyObject>) {
            let new_provider =
                read_draft().map(|draft| draft.0).unwrap_or(Provider::Gemini);
            apply_provider_change(new_provider);
        }

        #[unsafe(method(localModelChanged:))]
        fn local_model_changed(&self, _sender: Option<&AnyObject>) {
            render_local();
        }

        #[unsafe(method(advancedToggled:))]
        fn advanced_toggled(&self, _sender: Option<&AnyObject>) {
            refresh_advanced_visibility();
            layout_window();
        }

        #[unsafe(method(downloadClicked:))]
        fn download_clicked(&self, _sender: Option<&AnyObject>) {
            let Some(status) = self.ivars().status.get() else {
                return;
            };
            let selected = STATE.with(|state| {
                state.borrow().as_ref().map(|window_state| {
                    local_model_at(window_state.local_model_popup.indexOfSelectedItem())
                })
            });
            let Some(selected) = selected else { return };
            if let Err(error) = install::start_install(selected, Arc::new(|| {})) {
                set_status(status, &error, true);
                return;
            }
            render_local();
        }

        #[unsafe(method(cancelDownloadClicked:))]
        fn cancel_download_clicked(&self, _sender: Option<&AnyObject>) {
            install::cancel_install();
            render_local();
        }

        #[unsafe(method(deleteModelClicked:))]
        fn delete_model_clicked(&self, _sender: Option<&AnyObject>) {
            let Some(status) = self.ivars().status.get() else {
                return;
            };
            let selected = STATE.with(|state| {
                state.borrow().as_ref().map(|window_state| {
                    local_model_at(window_state.local_model_popup.indexOfSelectedItem())
                })
            });
            let Some(selected) = selected else { return };
            let label = catalog::model_file(selected).label;
            let Some(mtm) = MainThreadMarker::new() else {
                set_status(
                    status,
                    "Could not delete the model: Writely must ask on the main thread.",
                    true,
                );
                return;
            };
            // No STATE borrow is held across runModal: it spins a nested
            // run loop and the progress timer borrows STATE.
            let alert = NSAlert::new(mtm);
            alert.setMessageText(&NSString::from_str(&format!(
                "Delete {label}? You can download it again later."
            )));
            let delete_button = alert.addButtonWithTitle(&NSString::from_str("Delete"));
            drop(delete_button);
            let cancel_button = alert.addButtonWithTitle(&NSString::from_str("Cancel"));
            drop(cancel_button);
            if alert.runModal() != NSAlertFirstButtonReturn {
                return;
            }
            server::stop("model deleted");
            match install::delete_model(selected) {
                Ok(()) => {
                    set_status(status, "Deleted.", false);
                    super::notify_changed();
                }
                Err(error) => set_status(status, &error, true),
            }
            render_local();
        }

        #[unsafe(method(openAccessibilitySettings:))]
        fn open_accessibility_settings(&self, _sender: Option<&AnyObject>) {
            let Some(status) = self.ivars().status.get() else {
                return;
            };
            let url_string = NSString::from_str(super::ACCESSIBILITY_SETTINGS_URL);
            let Some(url) = NSURL::URLWithString(&url_string) else {
                set_status(
                    status,
                    "Could not open System Settings: invalid Accessibility settings URL",
                    true,
                );
                return;
            };
            if !NSWorkspace::sharedWorkspace().openURL(&url) {
                set_status(
                    status,
                    "Could not open System Settings: NSWorkspace could not open System Settings",
                    true,
                );
            }
        }
    }

    // SAFETY: NSWindowDelegate has no safety requirements.
    unsafe impl NSWindowDelegate for SettingsDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            STATE.with(|state| {
                if let Some(window_state) = state.borrow().as_ref() {
                    // Stop the progress timer; the window state (and with
                    // it the timer) is dropped just below.
                    window_state.local_timer.invalidate();
                    window_state.window.setDelegate(None);
                }
            });
            let taken = STATE.with(|state| state.borrow_mut().take());
            if let Some(taken) = taken {
                let taken_cell = std::cell::RefCell::new(Some(taken));
                let block = block2::RcBlock::new(move || {
                    drop(taken_cell.borrow_mut().take());
                });
                // SAFETY: the main run loop is valid and the block is copied by the run loop.
                unsafe { NSRunLoop::mainRunLoop().performBlock(&block) };
            }
        }
    }
);

impl SettingsDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars::default());
        // SAFETY: NSObject init signature is correct.
        unsafe { msg_send![super(this), init] }
    }
}

struct WindowState {
    window: Retained<NSWindow>,
    #[allow(dead_code)]
    delegate: Retained<SettingsDelegate>,
    field: Retained<NSSecureTextField>,
    status: Retained<NSTextField>,
    accessibility_button: Option<Retained<NSButton>>,
    provider_label: Retained<NSTextField>,
    provider_popup: Retained<NSPopUpButton>,
    provider_help: Retained<NSTextField>,
    model_label: Retained<NSTextField>,
    model_field: Retained<NSTextField>,
    model_help: Retained<NSTextField>,
    effort_label: Retained<NSTextField>,
    effort_popup: Retained<NSPopUpButton>,
    effort_help: Retained<NSTextField>,
    url_field: Retained<NSTextField>,
    key_label: Retained<NSTextField>,
    key_help: Retained<NSTextField>,
    link_button: Retained<NSButton>,
    url_label: Retained<NSTextField>,
    url_help: Retained<NSTextField>,
    local_model_label: Retained<NSTextField>,
    local_model_popup: Retained<NSPopUpButton>,
    local_model_help: Retained<NSTextField>,
    local_status: Retained<NSTextField>,
    local_download_button: Retained<NSButton>,
    local_cancel_button: Retained<NSButton>,
    local_delete_button: Retained<NSButton>,
    local_runtime_help: Retained<NSTextField>,
    local_warm_checkbox: Retained<NSButton>,
    local_warm_help: Retained<NSTextField>,
    local_idle_label: Retained<NSTextField>,
    local_idle_field: Retained<NSTextField>,
    local_idle_help: Retained<NSTextField>,
    local_advanced_checkbox: Retained<NSButton>,
    adv_labels: [Retained<NSTextField>; 5],
    adv_fields: [Retained<NSTextField>; 5],
    adv_helps: [Retained<NSTextField>; 5],
    local_timer: Retained<NSTimer>,
    local_runtime_error: bool,
    local_was_busy: bool,
    autostart_checkbox: Retained<NSButton>,
    autostart_help: Retained<NSTextField>,
    clipboard_checkbox: Retained<NSButton>,
    clipboard_help: Retained<NSTextField>,
    save_button: Retained<NSButton>,
    remove_button: Retained<NSButton>,
    hotkey_hint: Retained<NSTextField>,
    status_height: f64,
    settings: AppSettings,
    draft_keys: [String; 3],
}

thread_local! {
    static STATE: RefCell<Option<WindowState>> = const { RefCell::new(None) };
}

fn ensure_edit_menu(mtm: MainThreadMarker) {
    let app = NSApplication::sharedApplication(mtm);
    if let Some(main_menu) = app.mainMenu() {
        let count = main_menu.numberOfItems();
        for index in 0..count {
            if let Some(item) = main_menu.itemAtIndex(index) {
                if item.title().to_string() == "Edit" {
                    return;
                }
            }
        }
        let edit_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Edit"));
        // SAFETY: paste: and selectAll: are valid NSResponder selectors with no target.
        let paste = unsafe {
            edit_menu.addItemWithTitle_action_keyEquivalent(
                ns_string!("Paste"),
                Some(sel!(paste:)),
                ns_string!("v"),
            )
        };
        drop(paste);
        // SAFETY: paste: and selectAll: are valid NSResponder selectors with no target.
        let select_all = unsafe {
            edit_menu.addItemWithTitle_action_keyEquivalent(
                ns_string!("Select All"),
                Some(sel!(selectAll:)),
                ns_string!("a"),
            )
        };
        drop(select_all);
        let edit_item = NSMenuItem::new(mtm);
        edit_item.setTitle(ns_string!("Edit"));
        edit_item.setSubmenu(Some(&edit_menu));
        main_menu.addItem(&edit_item);
        return;
    }
    let main_menu = NSMenu::new(mtm);
    let edit_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Edit"));
    // SAFETY: paste: and selectAll: are valid NSResponder selectors with no target.
    let paste = unsafe {
        edit_menu.addItemWithTitle_action_keyEquivalent(
            ns_string!("Paste"),
            Some(sel!(paste:)),
            ns_string!("v"),
        )
    };
    drop(paste);
    // SAFETY: paste: and selectAll: are valid NSResponder selectors with no target.
    let select_all = unsafe {
        edit_menu.addItemWithTitle_action_keyEquivalent(
            ns_string!("Select All"),
            Some(sel!(selectAll:)),
            ns_string!("a"),
        )
    };
    drop(select_all);
    let edit_item = NSMenuItem::new(mtm);
    edit_item.setTitle(ns_string!("Edit"));
    edit_item.setSubmenu(Some(&edit_menu));
    main_menu.addItem(&edit_item);
    app.setMainMenu(Some(&main_menu));
}

pub fn show_settings() -> Result<(), String> {
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(
            "Could not open the settings window: Writely must open Settings on the main thread."
                .to_string(),
        );
    };
    ensure_edit_menu(mtm);

    let already_open = STATE.with(|state| state.borrow().is_some());
    if already_open {
        STATE.with(|state| {
            if let Some(window_state) = state.borrow().as_ref() {
                if let Some(message) = super::permission_message() {
                    set_status(&window_state.status, &message, true);
                    if let Some(button) = window_state.accessibility_button.as_ref() {
                        button.setHidden(false);
                    }
                } else {
                    if let Some(button) = window_state.accessibility_button.as_ref() {
                        button.setHidden(true);
                    }
                    let provider = provider_at(window_state.provider_popup.indexOfSelectedItem());
                    if provider.needs_key() {
                        match super::has_api_key_for(provider) {
                            Ok(true) => window_state
                                .field
                                .setPlaceholderString(Some(&NSString::from_str("••••••••"))),
                            Ok(false) => {
                                window_state
                                    .field
                                    .setPlaceholderString(Some(&NSString::from_str(
                                        "Paste your key here",
                                    )))
                            }
                            Err(error) => set_status(&window_state.status, &error, true),
                        }
                    }
                }
                if let Some(message) = super::provider_message() {
                    set_status(&window_state.status, &message, true);
                    super::set_provider_message(None);
                }
                NSApplication::sharedApplication(mtm).activate();
                window_state.window.makeKeyAndOrderFront(None);
            }
        });
        return Ok(());
    }

    let delegate = SettingsDelegate::new(mtm);
    let permission_text = super::permission_message();
    let window_height = if permission_text.is_some() {
        672.0
    } else {
        604.0
    };
    // SAFETY: the content rect, style mask and backing store are valid and mtm is the main thread.
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(420.0, window_height)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    // SAFETY: the window is owned by a Retained in STATE, so AppKit must not also release it on close.
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(ns_string!("Writely Settings"));
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    window.center();

    let Some(content) = window.contentView() else {
        return Err(
            "Could not open the settings window: the window has no content view.".to_string(),
        );
    };

    // Top-down layout: each row takes its height plus a gap, so nothing overlaps.
    let mut cursor = window_height - 16.0;
    let mut row = |height: f64, gap: f64| -> NSRect {
        cursor -= height;
        let rect = NSRect::new(NSPoint::new(20.0, cursor), NSSize::new(380.0, height));
        cursor -= gap;
        rect
    };

    let provider_label = NSTextField::labelWithString(ns_string!("Provider"), mtm);
    provider_label.setFrame(row(18.0, 4.0));

    // SAFETY: the frame is valid and mtm is the main thread.
    let provider_popup =
        NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), row(26.0, 2.0), false);
    for title in PROVIDER_TITLES {
        provider_popup.addItemWithTitle(&NSString::from_str(title));
    }

    let provider_help = NSTextField::labelWithString(ns_string!("Who checks your grammar."), mtm);
    provider_help.setFrame(row(16.0, 10.0));

    let model_label = NSTextField::labelWithString(ns_string!("Model"), mtm);
    model_label.setFrame(row(18.0, 4.0));

    let model_field = NSTextField::initWithFrame(NSTextField::alloc(mtm), row(24.0, 2.0));

    let model_help = NSTextField::wrappingLabelWithString(
        ns_string!("Paste the model name, e.g. gemini-3.5-flash-lite / deepseek-ai/DeepSeek-V4.1-Flash / llama3.2"),
        mtm,
    );
    model_help.setFrame(row(44.0, 10.0));

    let effort_label = NSTextField::labelWithString(ns_string!("Effort"), mtm);
    effort_label.setFrame(row(18.0, 4.0));

    // SAFETY: the frame is valid and mtm is the main thread.
    let effort_popup =
        NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), row(26.0, 2.0), false);
    for title in EFFORT_TITLES {
        effort_popup.addItemWithTitle(&NSString::from_str(title));
    }

    let effort_help = NSTextField::wrappingLabelWithString(
        ns_string!("How hard the model thinks. Not every model supports every level; you'll see the model's error if not."),
        mtm,
    );
    effort_help.setFrame(row(34.0, 10.0));

    // The key rows and the Ollama-address rows share the same frames; only one
    // set is visible at a time.
    let key_label = NSTextField::labelWithString(ns_string!("Google Gemini API key"), mtm);
    key_label.setFrame(row(18.0, 4.0));
    let key_frame = key_label.frame();

    let field = NSSecureTextField::initWithFrame(NSSecureTextField::alloc(mtm), row(24.0, 2.0));
    let key_field_frame = field.frame();

    let key_help = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
    key_help.setFrame(row(34.0, 2.0));
    let key_help_frame = key_help.frame();

    let target: &AnyObject = &delegate;
    // SAFETY: delegate implements openLink: and lives in STATE while the window is open.
    let link_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("https://aistudio.google.com/apikey"),
            Some(target),
            Some(sel!(openLink:)),
            mtm,
        )
    };
    link_button.setFrame(row(18.0, 10.0));
    link_button.setBordered(false);

    let url_label = NSTextField::labelWithString(ns_string!("Ollama address"), mtm);
    url_label.setFrame(key_frame);
    let url_field = NSTextField::initWithFrame(NSTextField::alloc(mtm), key_field_frame);
    let url_help = NSTextField::wrappingLabelWithString(
        ns_string!("Where Ollama runs on your computer. No key needed."),
        mtm,
    );
    url_help.setFrame(key_help_frame);

    // The "On this computer" section, shown only for that provider. Frames
    // are temporary: layout_window() positions everything once the window
    // state exists, and whenever visibility changes.
    let local_model_label = NSTextField::labelWithString(ns_string!("Model"), mtm);
    local_model_label.setFrame(key_help_frame);
    // SAFETY: the frame is valid and mtm is the main thread.
    let local_model_popup =
        NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), key_help_frame, false);
    for (_id, text) in super::local_model_choices() {
        local_model_popup.addItemWithTitle(&NSString::from_str(&text));
    }
    // SAFETY: delegate implements localModelChanged: and lives in STATE while the window is open.
    unsafe {
        local_model_popup.setTarget(Some(target));
        local_model_popup.setAction(Some(sel!(localModelChanged:)));
    }
    let local_model_help = NSTextField::wrappingLabelWithString(
        ns_string!(
            "Smaller is faster; larger makes fewer mistakes. Your text never leaves this computer."
        ),
        mtm,
    );
    local_model_help.setFrame(key_help_frame);
    let local_status = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
    local_status.setFrame(key_help_frame);
    // SAFETY: delegate implements downloadClicked: and lives in STATE while the window is open.
    let local_download_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Download"),
            Some(target),
            Some(sel!(downloadClicked:)),
            mtm,
        )
    };
    local_download_button.setFrame(key_help_frame);
    // SAFETY: delegate implements cancelDownloadClicked: and lives in STATE while the window is open.
    let local_cancel_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Cancel"),
            Some(target),
            Some(sel!(cancelDownloadClicked:)),
            mtm,
        )
    };
    local_cancel_button.setFrame(key_help_frame);
    // SAFETY: delegate implements deleteModelClicked: and lives in STATE while the window is open.
    let local_delete_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Delete model"),
            Some(target),
            Some(sel!(deleteModelClicked:)),
            mtm,
        )
    };
    local_delete_button.setFrame(key_help_frame);
    let (local_runtime_help_text, local_runtime_error) = match catalog::runtime_file() {
        Ok(runtime) => (
            format!(
                "The first download also gets the llama.cpp helper ({}). Files are saved in {}.",
                download::human_size(runtime.size),
                catalog::local_data_dir().display()
            ),
            false,
        ),
        Err(error) => (error, true),
    };
    let local_runtime_help =
        NSTextField::wrappingLabelWithString(&NSString::from_str(&local_runtime_help_text), mtm);
    local_runtime_help.setFrame(key_help_frame);
    // SAFETY: a checkbox with no target/action is valid; its state is read on Save.
    let local_warm_checkbox = unsafe {
        NSButton::checkboxWithTitle_target_action(
            ns_string!("Load the model when Writely starts"),
            None,
            None,
            mtm,
        )
    };
    local_warm_checkbox.setFrame(key_help_frame);
    let local_warm_help = NSTextField::wrappingLabelWithString(
        ns_string!("Makes the first check fast. While loaded it uses about 1 GB of memory (2 GB for the 2B model)."),
        mtm,
    );
    local_warm_help.setFrame(key_help_frame);
    let local_idle_label =
        NSTextField::labelWithString(ns_string!("Free memory after (minutes)"), mtm);
    local_idle_label.setFrame(key_help_frame);
    let local_idle_field = NSTextField::initWithFrame(NSTextField::alloc(mtm), key_help_frame);
    let local_idle_help = NSTextField::wrappingLabelWithString(
        ns_string!("Unload the model after this many minutes without use. 0 = never. It loads again in about a second when needed."),
        mtm,
    );
    local_idle_help.setFrame(key_help_frame);
    // SAFETY: delegate implements advancedToggled: and lives in STATE while the window is open.
    let local_advanced_checkbox = unsafe {
        NSButton::checkboxWithTitle_target_action(
            ns_string!("Show advanced options"),
            Some(target),
            Some(sel!(advancedToggled:)),
            mtm,
        )
    };
    local_advanced_checkbox.setFrame(key_help_frame);
    // "Show advanced options" starts unchecked every time Settings opens.
    local_advanced_checkbox.setState(0);
    let mut adv_label_vec: Vec<Retained<NSTextField>> = Vec::new();
    let mut adv_field_vec: Vec<Retained<NSTextField>> = Vec::new();
    let mut adv_help_vec: Vec<Retained<NSTextField>> = Vec::new();
    for i in 0..5 {
        let label = NSTextField::labelWithString(&NSString::from_str(ADVANCED_LABELS[i]), mtm);
        label.setFrame(key_help_frame);
        adv_label_vec.push(label);
        adv_field_vec.push(NSTextField::initWithFrame(
            NSTextField::alloc(mtm),
            key_help_frame,
        ));
        let help =
            NSTextField::wrappingLabelWithString(&NSString::from_str(ADVANCED_HELPS[i]), mtm);
        help.setFrame(key_help_frame);
        adv_help_vec.push(help);
    }
    let adv_labels: [Retained<NSTextField>; 5] =
        adv_label_vec.try_into().expect("five advanced labels");
    let adv_fields: [Retained<NSTextField>; 5] =
        adv_field_vec.try_into().expect("five advanced fields");
    let adv_helps: [Retained<NSTextField>; 5] =
        adv_help_vec.try_into().expect("five advanced helps");

    // SAFETY: a checkbox with no target/action is valid; its state is read on Save.
    let autostart_checkbox = unsafe {
        NSButton::checkboxWithTitle_target_action(
            ns_string!("Start Writely at login"),
            None,
            None,
            mtm,
        )
    };
    autostart_checkbox.setFrame(row(22.0, 2.0));

    let autostart_help = NSTextField::wrappingLabelWithString(
        ns_string!("Writely starts automatically when you log in."),
        mtm,
    );
    autostart_help.setFrame(row(16.0, 10.0));

    // SAFETY: a checkbox with no target/action is valid; its state is read on Save.
    let clipboard_checkbox = unsafe {
        NSButton::checkboxWithTitle_target_action(
            ns_string!("Use the clipboard when an app doesn't share its text"),
            None,
            None,
            mtm,
        )
    };
    clipboard_checkbox.setFrame(row(22.0, 2.0));

    let clipboard_help = NSTextField::wrappingLabelWithString(
        ns_string!("Writely copies your selection and then puts your clipboard back. Turn off to never touch the clipboard."),
        mtm,
    );
    clipboard_help.setFrame(row(34.0, 10.0));

    // SAFETY: delegate implements saveClicked: and lives in STATE while the window is open.
    let save_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Save"),
            Some(target),
            Some(sel!(saveClicked:)),
            mtm,
        )
    };
    let buttons_frame = row(26.0, 12.0);
    save_button.setFrame(NSRect::new(buttons_frame.origin, NSSize::new(80.0, 26.0)));

    // SAFETY: delegate implements removeClicked: and lives in STATE while the window is open.
    let remove_button = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Remove key"),
            Some(target),
            Some(sel!(removeClicked:)),
            mtm,
        )
    };
    remove_button.setFrame(NSRect::new(
        NSPoint::new(buttons_frame.origin.x + 90.0, buttons_frame.origin.y),
        NSSize::new(110.0, 26.0),
    ));

    let hotkey_hint = NSTextField::wrappingLabelWithString(
        ns_string!("Tip: select text in any app and press ⌃⌥G to check it."),
        mtm,
    );
    hotkey_hint.setFrame(row(18.0, 10.0));

    // The Accessibility button gets its own row above the status line and only
    // exists while the permission message is set. It uses its own selector.
    let accessibility_button: Option<Retained<NSButton>> = if permission_text.is_some() {
        // SAFETY: delegate implements openAccessibilitySettings: and lives in STATE while the window is open.
        let button = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("Open Accessibility Settings"),
                Some(target),
                Some(sel!(openAccessibilitySettings:)),
                mtm,
            )
        };
        button.setFrame(row(24.0, 8.0));
        content.addSubview(&button);
        Some(button)
    } else {
        None
    };

    let status_height = if permission_text.is_some() {
        54.0
    } else {
        18.0
    };
    let status_label = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
    status_label.setFrame(NSRect::new(
        NSPoint::new(20.0, 14.0),
        NSSize::new(380.0, status_height),
    ));

    // SAFETY: delegate implements providerChanged: and lives in STATE while the window is open.
    unsafe {
        provider_popup.setTarget(Some(target));
        provider_popup.setAction(Some(sel!(providerChanged:)));
    }

    // Load the draft settings: a missing file means defaults; an unreadable
    // or invalid file shows its error and starts from defaults (saving then
    // overwrites it, which is explicit, not silent).
    let mut initial_error: Option<String> = None;
    let settings = match crate::config::load_settings() {
        Ok(settings) => settings,
        Err(error) => {
            initial_error = Some(error);
            AppSettings::default()
        }
    };
    provider_popup.selectItemAtIndex(provider_index(settings.provider));
    effort_popup.selectItemAtIndex(effort_index(settings.effort));
    model_field.setStringValue(&NSString::from_str(&settings.model_for(settings.provider)));
    model_field.setPlaceholderString(Some(&NSString::from_str(model_placeholder_for(
        settings.provider,
    ))));
    autostart_checkbox.setState(if settings.launch_at_login { 1 } else { 0 });
    clipboard_checkbox.setState(if settings.clipboard_fallback { 1 } else { 0 });
    url_field.setStringValue(&NSString::from_str(&settings.ollama_url));
    url_field.setPlaceholderString(Some(&NSString::from_str(crate::config::DEFAULT_OLLAMA_URL)));
    key_help.setStringValue(&NSString::from_str(key_help_for(settings.provider)));
    key_label.setStringValue(&NSString::from_str(key_title_for(settings.provider)));
    if let Some(page) = super::api_key_page_for(settings.provider) {
        link_button.setTitle(&NSString::from_str(page));
    }
    local_model_popup.selectItemAtIndex(local_model_index(settings.local_model));
    local_warm_checkbox.setState(if settings.local_warm_up { 1 } else { 0 });
    local_idle_field.setStringValue(&NSString::from_str(
        &settings.local_idle_minutes.to_string(),
    ));
    let adv_values = [
        settings.local_threads,
        settings.local_max_reply_tokens,
        settings.local_context_tokens,
        settings.local_startup_timeout_secs,
        settings.local_reply_timeout_secs,
    ];
    for (i, value) in adv_values.into_iter().enumerate() {
        adv_fields[i].setStringValue(&NSString::from_str(&value.to_string()));
    }
    let is_ollama = settings.provider == Provider::Ollama;
    let is_local = settings.provider == Provider::Local;
    model_label.setHidden(is_local);
    model_field.setHidden(is_local);
    model_help.setHidden(is_local);
    local_model_label.setHidden(!is_local);
    local_model_popup.setHidden(!is_local);
    local_model_help.setHidden(!is_local);
    local_status.setHidden(!is_local);
    local_download_button.setHidden(!is_local);
    local_cancel_button.setHidden(!is_local);
    local_delete_button.setHidden(!is_local);
    local_runtime_help.setHidden(!is_local);
    local_warm_checkbox.setHidden(!is_local);
    local_warm_help.setHidden(!is_local);
    local_idle_label.setHidden(!is_local);
    local_idle_field.setHidden(!is_local);
    local_idle_help.setHidden(!is_local);
    local_advanced_checkbox.setHidden(!is_local);
    // Advanced rows start hidden: the checkbox is unchecked on open.
    for i in 0..5 {
        adv_labels[i].setHidden(true);
        adv_fields[i].setHidden(true);
        adv_helps[i].setHidden(true);
    }
    key_label.setHidden(is_ollama || is_local);
    field.setHidden(is_ollama || is_local);
    key_help.setHidden(is_ollama || is_local);
    link_button.setHidden(is_ollama || is_local);
    url_label.setHidden(!is_ollama);
    url_field.setHidden(!is_ollama);
    url_help.setHidden(!is_ollama);
    if settings.provider.needs_key() {
        match super::has_api_key_for(settings.provider) {
            Ok(true) => field.setPlaceholderString(Some(&NSString::from_str("••••••••"))),
            Ok(false) => {
                field.setPlaceholderString(Some(&NSString::from_str("Paste your key here")))
            }
            Err(error) => initial_error = Some(error),
        }
    }
    if let Some(message) = permission_text.as_ref() {
        set_status(&status_label, message, true);
    } else if let Some(error) = &initial_error {
        set_status(&status_label, error, true);
    } else if let Some(message) = super::provider_message() {
        set_status(&status_label, &message, true);
        super::set_provider_message(None);
    } else if let Some(hotkey_error) = super::hotkey_message() {
        set_status(&status_label, &hotkey_error, true);
    } else if let Some(autostart_error) = super::autostart_message() {
        set_status(&status_label, &autostart_error, true);
    }

    if delegate.ivars().field.set(field.clone()).is_err() {
        eprintln!("Writely: could not store the Settings key field.");
    }
    if delegate.ivars().status.set(status_label.clone()).is_err() {
        eprintln!("Writely: could not store the Settings status label.");
    }

    content.addSubview(&provider_label);
    content.addSubview(&provider_popup);
    content.addSubview(&provider_help);
    content.addSubview(&model_label);
    content.addSubview(&model_field);
    content.addSubview(&model_help);
    content.addSubview(&effort_label);
    content.addSubview(&effort_popup);
    content.addSubview(&effort_help);
    content.addSubview(&key_label);
    content.addSubview(&field);
    content.addSubview(&key_help);
    content.addSubview(&link_button);
    content.addSubview(&url_label);
    content.addSubview(&url_field);
    content.addSubview(&url_help);
    content.addSubview(&local_model_label);
    content.addSubview(&local_model_popup);
    content.addSubview(&local_model_help);
    content.addSubview(&local_status);
    content.addSubview(&local_download_button);
    content.addSubview(&local_cancel_button);
    content.addSubview(&local_delete_button);
    content.addSubview(&local_runtime_help);
    content.addSubview(&local_warm_checkbox);
    content.addSubview(&local_warm_help);
    content.addSubview(&local_idle_label);
    content.addSubview(&local_idle_field);
    content.addSubview(&local_idle_help);
    content.addSubview(&local_advanced_checkbox);
    for i in 0..5 {
        content.addSubview(&adv_labels[i]);
        content.addSubview(&adv_fields[i]);
        content.addSubview(&adv_helps[i]);
    }
    content.addSubview(&autostart_checkbox);
    content.addSubview(&autostart_help);
    content.addSubview(&clipboard_checkbox);
    content.addSubview(&clipboard_help);
    content.addSubview(&save_button);
    content.addSubview(&remove_button);
    content.addSubview(&hotkey_hint);
    content.addSubview(&status_label);

    // Re-render download progress every 0.25 s on the main run loop;
    // invalidated in windowWillClose before the state is dropped.
    let timer_block = block2::RcBlock::new(move |_timer: NonNull<NSTimer>| {
        render_local();
    });
    // SAFETY: the block captures nothing and only touches the main
    // thread's STATE; it runs on the main run loop.
    let local_timer =
        unsafe { NSTimer::scheduledTimerWithTimeInterval_repeats_block(0.25, true, &timer_block) };

    STATE.with(|state| {
        *state.borrow_mut() = Some(WindowState {
            window: window.clone(),
            delegate,
            field: field.clone(),
            status: status_label.clone(),
            accessibility_button,
            provider_label: provider_label.clone(),
            provider_popup: provider_popup.clone(),
            provider_help: provider_help.clone(),
            model_label: model_label.clone(),
            model_field: model_field.clone(),
            model_help: model_help.clone(),
            effort_label: effort_label.clone(),
            effort_popup: effort_popup.clone(),
            effort_help: effort_help.clone(),
            url_field: url_field.clone(),
            key_label: key_label.clone(),
            key_help: key_help.clone(),
            link_button: link_button.clone(),
            url_label: url_label.clone(),
            url_help: url_help.clone(),
            local_model_label: local_model_label.clone(),
            local_model_popup: local_model_popup.clone(),
            local_model_help: local_model_help.clone(),
            local_status: local_status.clone(),
            local_download_button: local_download_button.clone(),
            local_cancel_button: local_cancel_button.clone(),
            local_delete_button: local_delete_button.clone(),
            local_runtime_help: local_runtime_help.clone(),
            local_warm_checkbox: local_warm_checkbox.clone(),
            local_warm_help: local_warm_help.clone(),
            local_idle_label: local_idle_label.clone(),
            local_idle_field: local_idle_field.clone(),
            local_idle_help: local_idle_help.clone(),
            local_advanced_checkbox: local_advanced_checkbox.clone(),
            adv_labels: adv_labels.clone(),
            adv_fields: adv_fields.clone(),
            adv_helps: adv_helps.clone(),
            local_timer,
            local_runtime_error,
            local_was_busy: false,
            autostart_checkbox: autostart_checkbox.clone(),
            autostart_help: autostart_help.clone(),
            clipboard_checkbox: clipboard_checkbox.clone(),
            clipboard_help: clipboard_help.clone(),
            save_button: save_button.clone(),
            remove_button: remove_button.clone(),
            hotkey_hint: hotkey_hint.clone(),
            status_height,
            settings,
            draft_keys: [String::new(), String::new(), String::new()],
        });
    });

    render_local();
    layout_window();

    NSApplication::sharedApplication(mtm).activate();
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(Some(&field));
    Ok(())
}

pub fn is_settings_key_window() -> bool {
    STATE.with(|state| {
        state
            .borrow()
            .as_ref()
            .map(|window_state| window_state.window.isKeyWindow())
            .unwrap_or(false)
    })
}

/// Called on the main thread when Accessibility permission is granted while
/// the app is running: hides the Accessibility button in the open Settings
/// window and shows a green ready message. Returns true if a window was open.
pub(crate) fn notify_permission_granted() -> bool {
    let found = STATE.with(|state| {
        let borrow = state.borrow();
        borrow.as_ref().map(|window_state| {
            (
                window_state.accessibility_button.clone(),
                window_state.status.clone(),
            )
        })
    });
    let Some((button, status)) = found else {
        return false;
    };
    if let Some(button) = button.as_ref() {
        button.setHidden(true);
    }
    set_status(&status, "Accessibility is on — Writely is ready.", false);
    true
}
