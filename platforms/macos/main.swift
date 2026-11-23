// Hime input method — program entry point (SPEC_MACOS §1, §3.2).
//
// Initializes the Rust engine with the bundled dictionary, creates the
// IMKServer and the shared candidate panel, then spins the run loop.
// The process stays alive as a background IMK server (LSBackgroundOnly).

import Cocoa
import InputMethodKit

let connectionName = "Hime_1_Connection"
let bundleIdentifier = "com.hime.inputmethod.Hime"

// Shared candidate panel used by every HimeInputController instance.
var sharedCandidates: IMKCandidates!

guard let dictPath = Bundle.main.path(forResource: "liuli_table", ofType: "txt") else {
    fputs("Hime: liuli_table.txt not found in bundle Resources\n", stderr)
    exit(1)
}
guard hime_init(dictPath) else {
    fputs("Hime: hime_init failed for dictionary \(dictPath)\n", stderr)
    exit(1)
}

let server = IMKServer(name: connectionName, bundleIdentifier: bundleIdentifier)
guard server != nil else {
    fputs("Hime: failed to create IMKServer (\(connectionName))\n", stderr)
    exit(1)
}

sharedCandidates = IMKCandidates(server: server,
                                 panelType: kIMKSingleRowSteppingCandidatePanel)
// Route keystrokes to the controller (that is, to the Rust engine) before the
// panel sees them. By default the panel takes key events first and commits its
// own selection-key choice itself, which would leave the engine composing a
// stale encoding.
sharedCandidates.setAttributes([IMKCandidatesSendServerKeyEventFirst: NSNumber(value: true)])

NSApplication.shared.run()
