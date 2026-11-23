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
        if (97...122).contains(scalar) || (49...57).contains(scalar) { return scalar }
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
              !event.modifierFlags.contains(.control),
              let keyCode = mappedKeyCode(for: event)
        else { return false }

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
        let item = NSMenuItem(title: "Hime 首选项…",
                              action: #selector(PreferencesWindowController.showPreferences(_:)),
                              keyEquivalent: "")
        item.target = PreferencesWindowController.shared
        let menu = NSMenu(title: "Hime")
        menu.addItem(item)
        return menu
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
