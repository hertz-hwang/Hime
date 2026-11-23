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

    override func handle(_ event: NSEvent!, client sender: Any!) -> Bool {
        guard event.type == .keyDown,
              !event.modifierFlags.contains(.command),
              !event.modifierFlags.contains(.control),
              sessionId != 0,
              let keyCode = mappedKeyCode(for: event)
        else { return false }

        guard let jsonPtr = hime_process_key(sessionId, keyCode, 0) else { return false }
        defer { hime_free_string(jsonPtr) }
        let json = String(cString: jsonPtr)
        guard let response = parseResponse(json) else { return false }

        currentCandidates = response.candidates
        render(response, to: sender as? IMKTextInput)
        return response.handled
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
            sharedCandidates.update()
            sharedCandidates.show(kIMKLocateCandidatesBelowHint)
        }
    }

    // MARK: - Candidate panel

    override func candidates(_ sender: Any!) -> [Any]! {
        currentCandidates.map { $0.text }
    }
}
