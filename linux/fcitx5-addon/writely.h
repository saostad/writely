// Writely input method bridge for fcitx5.
//
// Talks to the Writely process over the session D-Bus using the protocol in
// docs/linux-ime-bridge.md: it reports focus and surrounding text, and (in
// later items) shows Writely's icon and suggestions in the input panel.
#ifndef WRITELY_H
#define WRITELY_H

#include <map>
#include <memory>
#include <set>
#include <string>
#include <vector>
#include <fcitx-utils/dbus/bus.h>
#include <fcitx-utils/dbus/message.h>
#include <fcitx-utils/dbus/servicewatcher.h>
#include <fcitx-utils/handlertable.h>
#include <fcitx/addoninstance.h>
#include <fcitx/candidatelist.h>
#include <fcitx/inputcontext.h>
#include <fcitx/instance.h>

class WritelyCandidateWord;
class WritelyCandidateList;

class Writely final : public fcitx::AddonInstance {
public:
    explicit Writely(fcitx::Instance *instance);
    ~Writely() override;

    Writely(const Writely &) = delete;
    Writely &operator=(const Writely &) = delete;

private:
    // Writely (dis)appeared on the bus.
    void onWritelyAppeared();
    void onWritelyDisappeared();

    // Input context events.
    void onFocusIn(fcitx::InputContext *ic);
    void onFocusOut(fcitx::InputContext *ic);
    void onSurroundingTextUpdated(fcitx::InputContext *ic);
    void onDestroyed(fcitx::InputContext *ic);

    // One FocusIn + SurroundingText pair for an already-tracked context.
    void announceFocus(fcitx::InputContext *ic);
    // SurroundingText, only when the context has the capability and valid text
    // and Writely already knows the context.
    void maybeSendSurroundingText(fcitx::InputContext *ic);

    // Fire-and-forget async calls; failures are logged, never thrown.
    void send(fcitx::dbus::Message msg, const std::string &method);

    // Writely signals.
    bool onSignal(fcitx::dbus::Message &msg);
    void onShowIcon(const std::string &ctx);
    void onShowMessage(const std::string &ctx, const std::string &text,
                       bool isError, bool canApply);
    void onHide(const std::string &ctx);
    // The user clicked one of Writely's candidates.
    void sendClicked(fcitx::InputContext *ic, const std::string &action);

    // True when the panel is free for Writely (no preedit, no foreign list).
    static bool panelAvailableForWritely(fcitx::InputContext *ic);
    static bool hasWritelyList(fcitx::InputContext *ic);
    // Remove Writely's list and aux text; returns true when anything changed.
    bool hideWritelyPanel(fcitx::InputContext *ic, const std::string &ctx);

    static std::string contextId(const fcitx::InputContext *ic);
    static std::string uuidToHex(const fcitx::ICUUID &uuid);
    static bool hexToUuid(const std::string &hex, fcitx::ICUUID &uuid);
    fcitx::InputContext *findContext(const std::string &ctx);

    fcitx::Instance *instance_;
    fcitx::dbus::Bus *bus_ = nullptr;
    std::unique_ptr<fcitx::dbus::ServiceWatcher> watcher_;
    std::unique_ptr<fcitx::dbus::ServiceWatcherEntry> watcherEntry_;
    std::unique_ptr<fcitx::dbus::Slot> signalMatch_;
    std::vector<std::unique_ptr<fcitx::HandlerTableEntry<fcitx::EventHandler>>>
        eventWatchers_;
    // Context ids Writely currently knows (FocusIn sent, not Destroyed).
    std::set<std::string> announced_;
    // What Writely content is currently shown per context id.
    struct ShownPanel {
        bool isMessage = false;
        std::string auxText;
    };
    std::map<std::string, ShownPanel> shown_;
    // Programs already warned about missing surrounding-text support.
    std::set<std::string> warnedPrograms_;
    fcitx::ICUUID focusedUuid_{};
    bool hasFocused_ = false;
    bool writelyAvailable_ = false;

    friend class ::WritelyCandidateWord;
};

// A clickable Writely candidate: selecting it sends Clicked(ctx, action).
class WritelyCandidateWord : public fcitx::CandidateWord {
public:
    WritelyCandidateWord(Writely *writely, const std::string &label,
                         const std::string &action);
    void select(fcitx::InputContext *inputContext) const override;

private:
    Writely *writely_;
    std::string action_;
};

// Marker subclass so Writely never touches another engine's candidate list.
class WritelyCandidateList : public fcitx::CommonCandidateList {};

#endif // WRITELY_H
