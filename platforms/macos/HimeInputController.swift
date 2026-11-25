// Hime input method — per-client input controller (SPEC_MACOS §3).
//
// All typing state lives in the Rust engine; this class only maps NSEvent
// keycodes to the hime-ffi key codes, renders the EngineResponse JSON
// (commit / preedit / candidates) and owns the session lifetime.

import Cocoa
import InputMethodKit

@objc(HimeInputController)
final class HimeInputController: IMKInputController, CandidatePanelDelegate {
    /// ASCII codes the engine maps onto candidate indexes 0-8 (SPEC §3:
    /// digits `1`-`N` select candidates on the current page, `N` being the
    /// configured page size, at most 9). A finished mouse selection is
    /// replayed to the engine as the matching digit.
    private let digitKeyCode: [UInt32] = Array(49...57) // "1" ... "9"

    /// Paging keys, replayed when the candidate window's page control is
    /// clicked (SPEC §2.1: `-` up, `=` down).
    private let pageUpKeyCode: UInt32 = 45 // "-"
    private let pageDownKeyCode: UInt32 = 61 // "="

    /// macOS Caps Lock — the 中/英 key of Chinese keyboards (SPEC_MACOS §3.4).
    private let capsLockKeyCode: UInt16 = 57

    private var sessionId: UInt64 = 0

    // MARK: - Lifecycle

    override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)
        sessionId = hime_create_session()
    }

    deinit {
        HimeCandidatesWindow.shared.hide(ifDelegate: self)
        if sessionId != 0 {
            hime_destroy_session(sessionId)
        }
    }

    /// A source switch (the 中/英 key), another app or another text field
    /// takes the session away: end any standing composition through the
    /// engine so a typed encoding is committed where it was typed instead of
    /// dangling as Marked Text across the switch. Idle sessions answer Pass,
    /// so this is harmless when there is nothing to commit.
    override func deactivateServer(_ sender: Any!) {
        endComposition(sender)
        HimeCandidatesWindow.shared.hide(ifDelegate: self)
        super.deactivateServer(sender)
    }

    /// The system wants the composition session to end (a click outside the
    /// composed text, an input-source switch): commit the encoding as typed
    /// and clear the Marked Text (SPEC_MACOS §3.4).
    override func commitComposition(_ sender: Any!) {
        endComposition(sender)
    }

    // MARK: - Key handling

    /// Whether the event is a letter key whatever Shift or Caps Lock add to
    /// it. `charactersIgnoringModifiers` skips Shift, so letters report a-z.
    private func isLetterKey(_ event: NSEvent) -> Bool {
        guard let scalar = event.charactersIgnoringModifiers?.unicodeScalars.first?.value
        else { return false }
        return (65...90).contains(scalar) || (97...122).contains(scalar)
    }

    /// Maps a macOS virtual key code / character to the ASCII key code
    /// accepted by hime_process_key (SPEC §2.1). Nil = unmapped.
    private func mappedKeyCode(for event: NSEvent) -> UInt32? {
        switch event.keyCode {
        case 36: return UInt32(HIME_KEY_ENTER)      // Return
        case 49: return UInt32(HIME_KEY_SPACE)      // Space
        case 51: return UInt32(HIME_KEY_BACKSPACE)  // Delete (Backspace)
        case 53: return UInt32(HIME_KEY_ESCAPE)     // Escape
        default: break
        }
        guard let scalar = event.charactersIgnoringModifiers?.unicodeScalars.first?.value
        else { return nil }
        if scalar >= 65, scalar <= 90 { return scalar + 32 } // A-Z -> a-z
        if (97...122).contains(scalar) { return scalar }
        // Other printable keys forward the *final* character, so shifted forms
        // (?, !, :, " …) reach the engine as their own ASCII code; digits are
        // covered by the same branch.
        if let final = event.characters?.unicodeScalars.first?.value, (33...126).contains(final) {
            return final
        }
        return nil
    }

    private func parseResponse(_ json: String) -> EngineResponse? {
        guard let data = json.data(using: .utf8) else { return nil }
        return try? JSONDecoder().decode(EngineResponse.self, from: data)
    }

    /// Sends one key to the engine and renders whatever it returns.
    @discardableResult
    private func sendKey(_ keyCode: UInt32, to client: IMKTextInput?) -> Bool {
        guard sessionId != 0, let jsonPtr = hime_process_key(sessionId, keyCode, 0) else {
            return false
        }
        defer { hime_free_string(jsonPtr) }
        guard let response = parseResponse(String(cString: jsonPtr)) else { return false }
        render(response, to: client)
        return response.handled
    }

    /// Ends the composition through the engine exactly as Enter would: the
    /// raw encoding goes to the host and the Marked Text is cleared. The
    /// engine owns whether there is anything to end, so an idle session
    /// answers Pass and this changes nothing (SPEC_MACOS §3.3/§3.4).
    private func endComposition(_ sender: Any?) {
        guard let client = (sender as? IMKTextInput) ?? client() else { return }
        sendKey(UInt32(HIME_KEY_ENTER), to: client)
    }

    /// Caps Lock (the 中/英 key) arrives as a flags-changed event in the
    /// language-switch configuration, so the controller must receive those
    /// as well as key-down events; the default mask is key-down only.
    override func recognizedEvents(_ sender: Any!) -> Int {
        let events: NSEvent.EventTypeMask = [.keyDown, .flagsChanged]
        return Int(events.rawValue)
    }

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        // Field diagnostic: the Caps Lock path is only reachable from real
        // hardware (macOS filters injected Caps Lock events), so the event
        // shape it delivers can only be observed here.
        if event.keyCode == capsLockKeyCode {
            NSLog("Hime: Caps Lock event type=%d capsLockFlag=%d",
                  event.type.rawValue, event.modifierFlags.contains(.capsLock) ? 1 : 0)
            // The 中/英 switch is the system's job (ABC source switch, gated
            // by TICapsLockLanguageSwitchCapable, SPEC_MACOS §2/§3.4) and
            // acts on a Caps Lock event the IME did not consume. The press
            // arrives as flags-changed when the system switches sources and
            // as key-down when it does not; either way, close any standing
            // composition through the engine exactly as Enter would so the
            // typed encoding is on screen before the switch, then release
            // the key untouched.
            if event.type == .keyDown || event.type == .flagsChanged {
                endComposition(sender)
            }
            return false
        }
        guard event.type == .keyDown,
              !event.modifierFlags.contains(.command),
              !event.modifierFlags.contains(.control)
        else { return false }

        if event.modifierFlags.contains(.capsLock) {
            // 全大写键入 (the switch's long-press state): Caps Lock is held
            // on inside Hime, so every key belongs to the host, which inserts
            // the uppercase characters itself. Folding letters to lowercase
            // into the engine here is what made uppercase untypeable while the
            // light was on.
            return false
        }
        if event.modifierFlags.contains(.shift), isLetterKey(event) {
            // Shift + letter = capital letter: like every macOS CJK input
            // method it never joins a lowercase encoding. The engine only
            // accepts lowercase, so the final capital is forwarded and
            // answered Pass (SPEC §3): the host still inserts it untouched
            // and a standing composition stays put — the round trip exists
            // so 英文/数字后英文标点 can see capitals (SPEC §3.1).
            guard let final = event.characters?.unicodeScalars.first?.value,
                  (65...90).contains(final)
            else { return false }
            return sendKey(final, to: sender as? IMKTextInput)
        }

        guard let keyCode = mappedKeyCode(for: event) else { return false }
        return sendKey(keyCode, to: sender as? IMKTextInput)
    }

    // MARK: - Rendering

    private func render(_ response: EngineResponse, to client: IMKTextInput?) {
        let notFound = NSRange(location: NSNotFound, length: NSNotFound)
        guard let client else { return }

        if let commit = response.commitText, !commit.isEmpty {
            client.insertText(commit, replacementRange: notFound)
        }
        if response.codeInWindow {
            // Non-embedded preedit: the code lives in the candidate window.
            // A one-space marked range keeps the host reporting a caret for
            // placing that window; the composition itself stays in Rust.
            if response.preedit.isEmpty {
                client.setMarkedText("", selectionRange: NSRange(location: 0, length: 0),
                                     replacementRange: notFound)
            } else {
                client.setMarkedText(" ", selectionRange: NSRange(location: 0, length: 1),
                                     replacementRange: notFound)
            }
        } else {
            // The display line may be the top candidate's word-separated
            // split; the raw encoding remains the engine's own state.
            let display = response.codeLine.isEmpty ? response.preedit : response.codeLine
            client.setMarkedText(display,
                                 selectionRange: NSRange(location: display.utf16.count, length: 0),
                                 replacementRange: notFound)
        }

        let showsCode = response.codeInWindow && !response.codeLine.isEmpty
        guard !response.candidates.isEmpty || showsCode else {
            HimeCandidatesWindow.shared.hide()
            return
        }
        HimeCandidatesWindow.shared.show(
            response,
            caret: caretRect(from: client),
            vertical: PreferencesModel.shared.verticalCandidates,
            fontSize: CGFloat(PreferencesModel.shared.candidateFontSize),
            delegate: self
        )
    }

    /// The caret line in global screen coordinates. The end of the marked
    /// range is where the next character goes, so the panel hangs from the
    /// insertion point rather than from the start of the composition. Clients
    /// answering with an empty rect are treated as unknown.
    private func caretRect(from client: IMKTextInput) -> NSRect? {
        let marked = client.markedRange()
        if marked.location != NSNotFound {
            let end = NSRange(location: marked.location + marked.length, length: 0)
            let rect = client.firstRect(forCharacterRange: end, actualRange: nil)
            if rect != .zero { return rect }
            let head = client.firstRect(forCharacterRange: marked, actualRange: nil)
            if head != .zero { return head }
        }
        let selection = client.selectedRange()
        if selection.location != NSNotFound {
            let rect = client.firstRect(forCharacterRange: selection, actualRange: nil)
            if rect != .zero { return rect }
        }
        return nil
    }

    // MARK: - Input menu

    /// Commands shown in the text input menu, in the section headed by the
    /// app's name group that the text input menu exposes.
    override func menu() -> NSMenu! {
        // IMK special-cases items whose action is `showPreferences:` and
        // invokes it on the controller; targets set on the item are ignored.
        let item = NSMenuItem(title: "Hime 首选项…",
                              action: #selector(showPreferences(_:)),
                              keyEquivalent: "")
        let menu = NSMenu(title: "Hime")
        menu.addItem(item)
        return menu
    }

    /// Overrides the default nib-loading implementation, which has nothing
    /// to load in this app.
    override func showPreferences(_ sender: Any?) {
        PreferencesWindowController.shared.showPreferences(sender)
    }

    // MARK: - Candidate panel

    /// A click in the panel is a mouse selection, not a key event: replay it
    /// to the engine as the matching digit so the Rust session stays the only
    /// owner of the composition.
    func candidatePanelDidSelect(_ index: Int) {
        guard index >= 0, index < digitKeyCode.count else { return }
        sendKey(digitKeyCode[index], to: client())
    }

    /// The page control replays the engine's paging keys.
    func candidatePanelDidPage(_ direction: Int) {
        let key = direction < 0 ? pageUpKeyCode : pageDownKeyCode
        sendKey(key, to: client())
    }
}
