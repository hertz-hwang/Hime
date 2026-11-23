// Hime input method — per-client input controller (SPEC_MACOS §3).
//
// All typing state lives in the Rust engine; this class only maps NSEvent
// keycodes to the hime-ffi key codes, renders the EngineResponse JSON
// (commit / preedit / candidates) and owns the session lifetime.

import Cocoa
import InputMethodKit

// JSON mirror of hime-core's EngineResponse (SPEC_MVP §2.2).
private struct Candidate: Decodable {
    let text: String
    let comment: String
}

private struct EngineResponse: Decodable {
    let handled: Bool
    let commitText: String?
    let preedit: String
    let candidates: [Candidate]

    enum CodingKeys: String, CodingKey {
        case handled
        case commitText = "commit_text"
        case preedit
        case candidates
    }
}

@objc(HimeInputController)
final class HimeInputController: IMKInputController {
    /// ASCII codes the engine maps onto candidate indexes 0-4 (SPEC_MVP §3:
    /// digits `1`-`5` select candidates 0-4). The engine caps the list at 5.
    private let digitKeyCode: [UInt32] = [49, 50, 51, 52, 53]

    /// macOS Caps Lock — the 中/英 key of Chinese keyboards (SPEC_MACOS §3.4).
    private let capsLockKeyCode: UInt16 = 57

    private var sessionId: UInt64 = 0
    private var currentCandidates: [Candidate] = []

    // MARK: - Lifecycle

    override init!(server: IMKServer!, delegate: Any!, client inputClient: Any!) {
        super.init(server: server, delegate: delegate, client: inputClient)
        sessionId = hime_create_session()
    }

    deinit {
        if sessionId != 0 {
            hime_destroy_session(sessionId)
        }
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
    /// accepted by hime_process_key (SPEC_MVP §2.1). Nil = unmapped.
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
        currentCandidates = response.candidates
        render(response, to: client)
        return response.handled
    }

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard event.type == .keyDown,
              !event.modifierFlags.contains(.command),
              !event.modifierFlags.contains(.control)
        else { return false }

        if event.keyCode == capsLockKeyCode {
            // The 中/英 switch itself is the system's job (ABC source switch,
            // enabled by TICapsLockLanguageSwitchCapable, SPEC_MACOS §3.4).
            // When the press still reaches Hime — the system switch is off, or
            // the key arrived before the source change — close any standing
            // composition through the engine exactly as Enter would, so no
            // Marked Text is left dangling, and keep the bare modifier press
            // out of the host.
            sendKey(UInt32(HIME_KEY_ENTER), to: sender as? IMKTextInput)
            return true
        }
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
            // method, the capital goes straight to the host and never into a
            // lowercase encoding; a standing composition and its Marked Text
            // stay untouched.
            return false
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
        if response.preedit.isEmpty {
            client.setMarkedText("", selectionRange: NSRange(location: 0, length: 0),
                                 replacementRange: notFound)
        } else {
            client.setMarkedText(response.preedit,
                                 selectionRange: NSRange(location: response.preedit.count, length: 0),
                                 replacementRange: notFound)
        }
        if response.candidates.isEmpty {
            sharedCandidates.hide()
        } else {
            // The preferences window owns the candidate font size; merging it
            // into the style attributes (which also carry the key-routing flag
            // set at startup) on every render lets a change show up as soon
            // as the panel updates.
            var attributes = sharedCandidates.attributes() ?? [:]
            attributes[NSAttributedString.Key.font] =
                NSFont.systemFont(ofSize: PreferencesModel.shared.candidateFontSize)
            sharedCandidates.setAttributes(attributes)
            sharedCandidates.update()
            sharedCandidates.show(kIMKLocateCandidatesBelowHint)
        }
    }

    // MARK: - Input menu

    /// Commands shown in the text input menu, in the section headed by the
    /// app's name — the layout 业火 exposes as its "Fire" group.
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

    override func candidates(_ sender: Any!) -> [Any]! {
        currentCandidates.map { $0.text }
    }

    /// The panel reports a finished mouse selection here, without a key event.
    /// Replay it to the engine as the matching digit instead of letting the
    /// IMK base class commit text, so the Rust session stays the only owner of
    /// the composition.
    override func candidateSelected(_ candidateString: NSAttributedString!) {
        let text = candidateString?.string
        guard let index = currentCandidates.firstIndex(where: { $0.text == text }),
              index < digitKeyCode.count
        else {
            NSLog("Hime: selected candidate %@ is not in the current list",
                  candidateString?.string ?? "<nil>")
            return
        }
        sendKey(digitKeyCode[index], to: client())
    }
}
