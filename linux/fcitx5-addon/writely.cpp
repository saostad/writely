// Writely input method bridge for fcitx5: lifecycle, focus and surrounding text.
#include "writely.h"

#include <cstdint>
#include <ctime>
#include <fcitx-utils/capabilityflags.h>
#include <fcitx-utils/dbus/matchrule.h>
#include <fcitx-utils/event.h>
#include <fcitx-utils/log.h>
#include <fcitx-utils/utf8.h>
#include <fcitx-module/clipboard/clipboard_public.h>
#include <fcitx-module/dbus/dbus_public.h>
#include <fcitx/addonfactory.h>
#include <fcitx/addonmanager.h>
#include <fcitx/event.h>
#include <fcitx/inputcontextmanager.h>
#include <fcitx/inputpanel.h>
#include <fcitx/instance.h>
#include <fcitx/surroundingtext.h>
#include <fcitx/text.h>
#include <fcitx/userinterface.h>

namespace {

constexpr const char *kBusName = "dev.writely.Writely";
constexpr const char *kObjectPath = "/dev/writely/ImeBridge";
constexpr const char *kInterface = "dev.writely.ImeBridge1";
constexpr const char *kBridgeName = "fcitx5";
constexpr const char *kAddonVersion = "0.1.0";
// Timeout for async D-Bus calls, in microseconds.
constexpr uint64_t kCallTimeoutUsec = 5000000;
// Delay after FocusIn before reporting missing surrounding text, giving
// the app time to send it. In microseconds.
constexpr uint64_t kUnavailableDelayUsec = 300000;

bool hasSurroundingText(const fcitx::InputContext *ic) {
    return ic->capabilityFlags().test(fcitx::CapabilityFlag::SurroundingText);
}

bool isPassword(const fcitx::InputContext *ic) {
    const auto flags = ic->capabilityFlags();
    return flags.test(fcitx::CapabilityFlag::Password) ||
           flags.test(fcitx::CapabilityFlag::Sensitive);
}

} // namespace

Writely::Writely(fcitx::Instance *instance) : instance_(instance) {
    eventWatchers_.push_back(instance_->watchEvent(
        fcitx::EventType::InputContextFocusIn,
        fcitx::EventWatcherPhase::Default, [this](fcitx::Event &event) {
            onFocusIn(static_cast<fcitx::FocusInEvent &>(event).inputContext());
        }));
    eventWatchers_.push_back(instance_->watchEvent(
        fcitx::EventType::InputContextFocusOut,
        fcitx::EventWatcherPhase::Default, [this](fcitx::Event &event) {
            onFocusOut(static_cast<fcitx::FocusOutEvent &>(event).inputContext());
        }));
    eventWatchers_.push_back(instance_->watchEvent(
        fcitx::EventType::InputContextSurroundingTextUpdated,
        fcitx::EventWatcherPhase::Default, [this](fcitx::Event &event) {
            onSurroundingTextUpdated(
                static_cast<fcitx::SurroundingTextUpdatedEvent &>(event)
                    .inputContext());
        }));
    eventWatchers_.push_back(instance_->watchEvent(
        fcitx::EventType::InputContextDestroyed,
        fcitx::EventWatcherPhase::Default, [this](fcitx::Event &event) {
            onDestroyed(static_cast<fcitx::InputContextDestroyedEvent &>(event)
                            .inputContext());
        }));

    fcitx::AddonInstance *dbusAddon =
        instance_->addonManager().addon("dbus");
    if (!dbusAddon) {
        FCITX_WARN() << "Writely: the fcitx5 dbus add-on is not available; the "
                        "Writely add-on is idle.";
        return;
    }
    bus_ = dbusAddon->call<fcitx::IDBusModule::bus>();
    if (!bus_) {
        FCITX_WARN() << "Writely: could not get the D-Bus connection from the "
                        "fcitx5 dbus add-on; the Writely add-on is idle.";
        return;
    }
    watcher_ = std::make_unique<fcitx::dbus::ServiceWatcher>(*bus_);
    watcherEntry_ = watcher_->watchService(
        kBusName,
        [this](const std::string & /*name*/, const std::string & /*oldOwner*/,
               const std::string &newOwner) {
            if (newOwner.empty()) {
                onWritelyDisappeared();
            } else {
                onWritelyAppeared();
            }
        });
    signalMatch_ = bus_->addMatch(
        fcitx::dbus::MatchRule(kBusName, kObjectPath, kInterface),
        [this](fcitx::dbus::Message &msg) { return onSignal(msg); });
}

Writely::~Writely() = default;

void Writely::onWritelyAppeared() {
    writelyAvailable_ = true;
    fcitx::dbus::Message hello =
        bus_->createMethodCall(kBusName, kObjectPath, kInterface, "Hello");
    hello << kBridgeName << kAddonVersion;
    send(std::move(hello), "Hello");
    if (hasFocused_) {
        if (fcitx::InputContext *ic =
                instance_->inputContextManager().findByUUID(focusedUuid_)) {
            announceFocus(ic);
        } else {
            hasFocused_ = false;
        }
    }
}

void Writely::onWritelyDisappeared() {
    writelyAvailable_ = false;
    announced_.clear();
    unavailableTimers_.clear();
    selectedText_.clear();
    // primaryAtFocus_ survives: tracked focus does too, and a selection made
    // while Writely was away is still the user's selection in that field.
    // Drop pending panel state; clicking a stale candidate would send Clicked
    // to nobody.
    instance_->inputContextManager().foreach([this](fcitx::InputContext *ic) {
        if (hideWritelyPanel(ic, contextId(ic))) {
            ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
        }
        return true;
    });
    shown_.clear();
    FCITX_INFO() << "Writely is not running; the Writely add-on is idle.";
}

void Writely::onFocusIn(fcitx::InputContext *ic) {
    focusedUuid_ = ic->uuid();
    hasFocused_ = true;
    recordPrimaryAtFocus(ic);
    announceFocus(ic);
}

void Writely::recordPrimaryAtFocus(fcitx::InputContext *ic) {
    const std::string ctx = contextId(ic);
    fcitx::AddonInstance *clipboard =
        instance_->addonManager().addon("clipboard");
    if (!clipboard) {
        primaryAtFocus_[ctx] = "";
        return;
    }
    try {
        primaryAtFocus_[ctx] =
            clipboard->call<fcitx::IClipboard::primary>(ic);
    } catch (const std::exception &e) {
        FCITX_WARN() << "Writely: could not read the PRIMARY selection for "
                        "focus tracking: "
                     << e.what();
        primaryAtFocus_[ctx] = "";
    } catch (...) {
        FCITX_WARN() << "Writely: could not read the PRIMARY selection for "
                        "focus tracking: unknown error";
        primaryAtFocus_[ctx] = "";
    }
}

void Writely::clearPrimaryAfterUse(fcitx::InputContext *ic,
                                   const std::string &ctx) {
    fcitx::AddonInstance *clipboard =
        instance_->addonManager().addon("clipboard");
    if (!clipboard) {
        return;
    }
    try {
        clipboard->call<fcitx::IClipboard::setPrimary>("writely", "");
        const std::string current =
            clipboard->call<fcitx::IClipboard::primary>(ic);
        if (!current.empty()) {
            FCITX_WARN() << "Writely: could not clear the PRIMARY selection "
                            "after use; it still holds "
                         << fcitx::utf8::length(current) << " characters.";
        }
        primaryAtFocus_[ctx] = current;
        selectedText_.erase(ctx);
    } catch (const std::exception &e) {
        FCITX_WARN() << "Writely: could not clear the PRIMARY selection "
                        "after use: "
                     << e.what();
    } catch (...) {
        FCITX_WARN() << "Writely: could not clear the PRIMARY selection "
                        "after use: unknown error";
    }
}

void Writely::onFocusOut(fcitx::InputContext *ic) {
    if (hasFocused_ && ic->uuid() == focusedUuid_) {
        hasFocused_ = false;
    }
    unavailableTimers_.erase(contextId(ic));
    if (!bus_) {
        return;
    }
    fcitx::dbus::Message msg =
        bus_->createMethodCall(kBusName, kObjectPath, kInterface, "FocusOut");
    msg << contextId(ic);
    send(std::move(msg), "FocusOut");
}

void Writely::onSurroundingTextUpdated(fcitx::InputContext *ic) {
    maybeSendSurroundingText(ic);
}

void Writely::onDestroyed(fcitx::InputContext *ic) {
    const std::string ctx = contextId(ic);
    if (hasFocused_ && ic->uuid() == focusedUuid_) {
        hasFocused_ = false;
    }
    announced_.erase(ctx);
    shown_.erase(ctx);
    unavailableTimers_.erase(ctx);
    primaryAtFocus_.erase(ctx);
    selectedText_.erase(ctx);
    if (bus_) {
        fcitx::dbus::Message msg =
            bus_->createMethodCall(kBusName, kObjectPath, kInterface, "Destroy");
        msg << ctx;
        send(std::move(msg), "Destroy");
    }
}

void Writely::announceFocus(fcitx::InputContext *ic) {
    if (!bus_) {
        return;
    }
    const std::string ctx = contextId(ic);
    fcitx::dbus::Message focusIn =
        bus_->createMethodCall(kBusName, kObjectPath, kInterface, "FocusIn");
    focusIn << ctx << ic->program() << isPassword(ic);
    send(std::move(focusIn), "FocusIn");
    announced_.insert(ctx);
    if (!hasSurroundingText(ic)) {
        if (warnedPrograms_.insert(ic->program()).second) {
            FCITX_WARN() << "Writely: " << ic->program()
                         << " doesn't share its text with input methods, so "
                            "Writely can't check it.";
        }
        return;
    }
    if (ic->surroundingText().isValid()) {
        maybeSendSurroundingText(ic);
    } else {
        // The app may still send its text; report it missing only if it
        // hasn't arrived after a short delay.
        scheduleUnavailableTimer(ic);
    }
}

bool Writely::clipboardAvailable() {
    return instance_->addonManager().addon("clipboard") != nullptr;
}

void Writely::scheduleUnavailableTimer(fcitx::InputContext *ic) {
    const std::string ctx = contextId(ic);
    const fcitx::ICUUID uuid = ic->uuid();
    auto timer = instance_->eventLoop().addTimeEvent(
        CLOCK_MONOTONIC,
        fcitx::now(CLOCK_MONOTONIC) + kUnavailableDelayUsec, 0,
        [this, ctx, uuid](fcitx::EventSourceTime *, uint64_t) {
            const fcitx::InputContext *current =
                instance_->inputContextManager().findByUUID(uuid);
            if (!current || !announced_.count(ctx) || !hasFocused_ ||
                current->uuid() != focusedUuid_) {
                return true;
            }
            if (hasSurroundingText(current) &&
                current->surroundingText().isValid()) {
                return true;
            }
            sendTextUnavailable(ctx, clipboardAvailable());
            return true;
        });
    timer->setOneShot();
    unavailableTimers_[ctx] = std::move(timer);
}

void Writely::sendTextUnavailable(const std::string &ctx, bool canSelect) {
    if (!bus_) {
        return;
    }
    fcitx::dbus::Message msg = bus_->createMethodCall(
        kBusName, kObjectPath, kInterface, "TextUnavailable");
    msg << ctx << canSelect;
    send(std::move(msg), "TextUnavailable");
}

void Writely::sendSelected(const std::string &ctx, uint32_t request, bool ok,
                           const std::string &text, const std::string &error) {
    if (!bus_) {
        return;
    }
    fcitx::dbus::Message msg = bus_->createMethodCall(
        kBusName, kObjectPath, kInterface, "Selected");
    msg << ctx << request << ok << text << error;
    send(std::move(msg), "Selected");
}

void Writely::maybeSendSurroundingText(fcitx::InputContext *ic) {
    if (!bus_) {
        return;
    }
    const std::string ctx = contextId(ic);
    // Writely answers SurroundingText for an unknown context with an error, so
    // only send for contexts it knows (avoids error spam while it is away).
    if (!announced_.count(ctx)) {
        return;
    }
    if (!hasSurroundingText(ic)) {
        return;
    }
    const fcitx::SurroundingText &surr = ic->surroundingText();
    if (!surr.isValid()) {
        return;
    }
    unavailableTimers_.erase(ctx);
    fcitx::dbus::Message msg = bus_->createMethodCall(
        kBusName, kObjectPath, kInterface, "SurroundingText");
    msg << ctx << surr.text() << surr.cursor() << surr.anchor();
    send(std::move(msg), "SurroundingText");
}

void Writely::send(fcitx::dbus::Message msg, const std::string &method) {
    if (!writelyAvailable_ || !bus_) {
        return;
    }
    // The reply slot must stay alive until the reply arrives; the callback
    // releases it via the shared holder.
    auto holder = std::make_shared<std::unique_ptr<fcitx::dbus::Slot>>();
    *holder = msg.callAsync(kCallTimeoutUsec,
                            [holder, method](fcitx::dbus::Message &reply) {
                                holder->reset();
                                if (reply.isError()) {
                                    FCITX_WARN()
                                        << "Writely: " << method
                                        << " failed: " << reply.errorName()
                                        << ": " << reply.errorMessage();
                                }
                                return true;
                            });
}

std::string Writely::contextId(const fcitx::InputContext *ic) {
    return uuidToHex(ic->uuid());
}

std::string Writely::uuidToHex(const fcitx::ICUUID &uuid) {
    static constexpr char kHex[] = "0123456789abcdef";
    std::string out;
    out.reserve(32);
    for (uint8_t byte : uuid) {
        out.push_back(kHex[byte >> 4]);
        out.push_back(kHex[byte & 0xF]);
    }
    return out;
}

bool Writely::hexToUuid(const std::string &hex, fcitx::ICUUID &uuid) {
    if (hex.size() != 32) {
        return false;
    }
    auto nibble = [](char c) -> int {
        if (c >= '0' && c <= '9') {
            return c - '0';
        }
        if (c >= 'a' && c <= 'f') {
            return c - 'a' + 10;
        }
        if (c >= 'A' && c <= 'F') {
            return c - 'A' + 10;
        }
        return -1;
    };
    for (size_t i = 0; i < 16; i++) {
        int hi = nibble(hex[2 * i]);
        int lo = nibble(hex[2 * i + 1]);
        if (hi < 0 || lo < 0) {
            return false;
        }
        uuid[i] = static_cast<uint8_t>((hi << 4) | lo);
    }
    return true;
}

fcitx::InputContext *Writely::findContext(const std::string &ctx) {
    fcitx::ICUUID uuid{};
    if (!hexToUuid(ctx, uuid)) {
        return nullptr;
    }
    return instance_->inputContextManager().findByUUID(uuid);
}

bool Writely::onSignal(fcitx::dbus::Message &msg) {
    const std::string member = msg.member();
    if (member != "ShowIcon" && member != "ShowMessage" && member != "Hide" &&
        member != "Replace" && member != "SelectRequest" &&
        member != "ReplaceSelection") {
        return true;
    }
    std::string ctx;
    msg >> ctx;
    if (!msg) {
        FCITX_WARN() << "Writely: ignoring a malformed " << member
                     << " signal from Writely.";
        return true;
    }
    if (member == "ShowIcon") {
        onShowIcon(ctx);
    } else if (member == "Hide") {
        onHide(ctx);
    } else if (member == "ShowMessage") {
        std::string text;
        bool isError = false;
        bool canApply = false;
        msg >> text >> isError >> canApply;
        if (!msg) {
            FCITX_WARN() << "Writely: ignoring a malformed ShowMessage signal "
                            "from Writely.";
            return true;
        }
        onShowMessage(ctx, text, isError, canApply);
    } else if (member == "Replace") {
        uint32_t request = 0;
        uint32_t start = 0;
        uint32_t end = 0;
        std::string text;
        msg >> request >> start >> end >> text;
        if (!msg) {
            FCITX_WARN() << "Writely: ignoring a malformed Replace signal "
                            "from Writely.";
            return true;
        }
        onReplace(ctx, request, start, end, text);
    } else if (member == "SelectRequest") {
        uint32_t request = 0;
        msg >> request;
        if (!msg) {
            FCITX_WARN() << "Writely: ignoring a malformed SelectRequest "
                            "signal from Writely.";
            return true;
        }
        onSelectRequest(ctx, request);
    } else if (member == "ReplaceSelection") {
        uint32_t request = 0;
        std::string text;
        msg >> request >> text;
        if (!msg) {
            FCITX_WARN() << "Writely: ignoring a malformed ReplaceSelection "
                            "signal from Writely.";
            return true;
        }
        onReplaceSelection(ctx, request, text);
    }
    return true;
}

bool Writely::panelAvailableForWritely(fcitx::InputContext *ic) {
    const fcitx::InputPanel &panel = ic->inputPanel();
    if (!panel.preedit().empty() || !panel.clientPreedit().empty()) {
        return false;
    }
    auto list = panel.candidateList();
    return !list || dynamic_cast<WritelyCandidateList *>(list.get()) != nullptr;
}

bool Writely::hasWritelyList(fcitx::InputContext *ic) {
    auto list = ic->inputPanel().candidateList();
    return list && dynamic_cast<WritelyCandidateList *>(list.get()) != nullptr;
}

bool Writely::hideWritelyPanel(fcitx::InputContext *ic, const std::string &ctx) {
    bool changed = false;
    fcitx::InputPanel &panel = ic->inputPanel();
    if (hasWritelyList(ic)) {
        panel.setCandidateList(std::unique_ptr<fcitx::CandidateList>());
        changed = true;
    }
    auto shown = shown_.find(ctx);
    if (shown != shown_.end() && shown->second.isMessage &&
        panel.auxUp().toString() == shown->second.auxText) {
        panel.setAuxUp(fcitx::Text());
        changed = true;
    }
    shown_.erase(ctx);
    return changed;
}

void Writely::onShowIcon(const std::string &ctx) {
    fcitx::InputContext *ic = findContext(ctx);
    if (!ic) {
        return;
    }
    // Idempotent: ShowIcon repeats on every caret change.
    auto shown = shown_.find(ctx);
    if (shown != shown_.end() && !shown->second.isMessage && hasWritelyList(ic)) {
        return;
    }
    if (!panelAvailableForWritely(ic)) {
        return;
    }
    auto list = std::make_unique<WritelyCandidateList>();
    // No selection keys: typed digits must never trigger a candidate.
    list->setSelectionKey({});
    list->append(
        std::make_unique<WritelyCandidateWord>(this, "W  Check grammar", "check"));
    // Replacing our own message also drops its aux text.
    hideWritelyPanel(ic, ctx);
    ic->inputPanel().setCandidateList(std::move(list));
    shown_[ctx] = ShownPanel{false, ""};
    ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
}

void Writely::onShowMessage(const std::string &ctx, const std::string &text,
                            bool isError, bool canApply) {
    fcitx::InputContext *ic = findContext(ctx);
    if (!ic) {
        return;
    }
    if (!panelAvailableForWritely(ic)) {
        FCITX_WARN() << "Writely: the input panel is in use by the input "
                        "method; skipped a Writely message.";
        return;
    }
    const std::string aux =
        (isError ? std::string("Writely error: ") : std::string("Writely: ")) +
        text;
    auto list = std::make_unique<WritelyCandidateList>();
    // No selection keys: typed digits must never trigger a candidate.
    list->setSelectionKey({});
    if (canApply) {
        list->append(
            std::make_unique<WritelyCandidateWord>(this, "Apply", "apply"));
    }
    list->append(
        std::make_unique<WritelyCandidateWord>(this, "Dismiss", "dismiss"));
    hideWritelyPanel(ic, ctx);
    ic->inputPanel().setAuxUp(fcitx::Text(aux));
    ic->inputPanel().setCandidateList(std::move(list));
    shown_[ctx] = ShownPanel{true, aux};
    ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
}

void Writely::onHide(const std::string &ctx) {
    fcitx::InputContext *ic = findContext(ctx);
    if (!ic) {
        shown_.erase(ctx);
        return;
    }
    if (hideWritelyPanel(ic, ctx)) {
        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
    }
}

void Writely::onReplace(const std::string &ctx, uint32_t request,
                         uint32_t start, uint32_t end, const std::string &text) {
    fcitx::InputContext *ic = findContext(ctx);
    if (!ic || !ic->surroundingText().isValid()) {
        sendReplaceResult(ctx, request, false, "the text field is gone");
        return;
    }
    const uint32_t cursor = ic->surroundingText().cursor();
    if (!(start <= cursor && cursor <= end)) {
        sendReplaceResult(ctx, request, false,
                          "the cursor moved outside the text to replace; click "
                          "inside the sentence and try again");
        return;
    }
    try {
        ic->deleteSurroundingText(static_cast<int>(start) -
                                      static_cast<int>(cursor),
                                  end - start);
        ic->commitString(text);
    } catch (const std::exception &e) {
        sendReplaceResult(ctx, request, false, e.what());
        return;
    } catch (...) {
        sendReplaceResult(ctx, request, false,
                          "unknown error while replacing the text");
        return;
    }
    sendReplaceResult(ctx, request, true, "");
    if (hideWritelyPanel(ic, ctx)) {
        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
    }
}

void Writely::onSelectRequest(const std::string &ctx, uint32_t request) {
    fcitx::InputContext *ic = findContext(ctx);
    if (!ic || !hasFocused_ || ic->uuid() != focusedUuid_) {
        sendSelected(ctx, request, false, "",
                     "the text field is no longer focused");
        return;
    }
    // The user selects; the add-on never changes the selection (forwarded
    // keys lose their modifiers on Wayland, so it can't select reliably).
    // Selecting sets the PRIMARY selection on Linux, so reading it back
    // never touches the user's clipboard. PRIMARY is only accepted when it
    // changed while this field had focus, so a selection from another app
    // is never reported.
    fcitx::AddonInstance *clipboard =
        instance_->addonManager().addon("clipboard");
    if (!clipboard) {
        sendSelected(ctx, request, false, "",
                     "the fcitx5 clipboard add-on is not available");
        return;
    }
    std::string current;
    try {
        current = clipboard->call<fcitx::IClipboard::primary>(ic);
    } catch (const std::exception &e) {
        sendSelected(ctx, request, false, "", e.what());
        return;
    } catch (...) {
        sendSelected(ctx, request, false, "",
                     "unknown error while reading the selection");
        return;
    }
    const auto baseline = primaryAtFocus_.find(ctx);
    if (current.empty() || (baseline != primaryAtFocus_.end() &&
                            current == baseline->second)) {
        sendSelected(ctx, request, true, "", "");
        return;
    }
    selectedText_[ctx] = current;
    sendSelected(ctx, request, true, current, "");
}

void Writely::onReplaceSelection(const std::string &ctx, uint32_t request,
                                 const std::string &text) {
    fcitx::InputContext *ic = findContext(ctx);
    if (!ic) {
        sendReplaceResult(ctx, request, false, "the text field is gone");
        return;
    }
    fcitx::AddonInstance *clipboard =
        instance_->addonManager().addon("clipboard");
    if (!clipboard) {
        sendReplaceResult(ctx, request, false,
                          "the fcitx5 clipboard add-on is not available");
        return;
    }
    std::string current;
    try {
        current = clipboard->call<fcitx::IClipboard::primary>(ic);
    } catch (const std::exception &e) {
        sendReplaceResult(ctx, request, false, e.what());
        return;
    } catch (...) {
        sendReplaceResult(ctx, request, false,
                          "unknown error while reading the selection");
        return;
    }
    const auto remembered = selectedText_.find(ctx);
    if (remembered == selectedText_.end() || current != remembered->second) {
        sendReplaceResult(ctx, request, false,
                          "the selection changed since Writely checked it; "
                          "click W again");
        return;
    }
    selectedText_.erase(ctx);
    try {
        // Committing over the still-active selection replaces it.
        ic->commitString(text);
    } catch (const std::exception &e) {
        sendReplaceResult(ctx, request, false, e.what());
        return;
    } catch (...) {
        sendReplaceResult(ctx, request, false,
                          "unknown error while replacing the text");
        return;
    }
    sendReplaceResult(ctx, request, true, "");
    // PRIMARY still holds the committed-over selection; clear it so
    // re-selecting the same text counts as new. This also resets the
    // baseline and drops the remembered selection.
    clearPrimaryAfterUse(ic, ctx);
    if (hideWritelyPanel(ic, ctx)) {
        ic->updateUserInterface(fcitx::UserInterfaceComponent::InputPanel);
    }
}

void Writely::sendReplaceResult(const std::string &ctx, uint32_t request,
                                bool ok, const std::string &error) {
    if (!bus_) {
        return;
    }
    fcitx::dbus::Message msg = bus_->createMethodCall(
        kBusName, kObjectPath, kInterface, "ReplaceResult");
    msg << ctx << request << ok << error;
    send(std::move(msg), "ReplaceResult");
}

void Writely::sendClicked(fcitx::InputContext *ic, const std::string &action) {
    if (!bus_) {
        return;
    }
    fcitx::dbus::Message msg =
        bus_->createMethodCall(kBusName, kObjectPath, kInterface, "Clicked");
    msg << contextId(ic) << action;
    send(std::move(msg), "Clicked");
    if (action == "dismiss") {
        const std::string ctx = contextId(ic);
        if (selectedText_.count(ctx)) {
            clearPrimaryAfterUse(ic, ctx);
        }
    }
}

WritelyCandidateWord::WritelyCandidateWord(Writely *writely,
                                           const std::string &label,
                                           const std::string &action)
    : fcitx::CandidateWord(fcitx::Text(label)), writely_(writely),
      action_(action) {}

void WritelyCandidateWord::select(fcitx::InputContext *inputContext) const {
    writely_->sendClicked(inputContext, action_);
}

class WritelyFactory : public fcitx::AddonFactory {
public:
    fcitx::AddonInstance *create(fcitx::AddonManager *manager) override {
        return new Writely(manager->instance());
    }
};

FCITX_ADDON_FACTORY_V2(writely, WritelyFactory);
