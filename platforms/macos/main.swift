// Hime input method — program entry point (SPEC_MACOS §1, §3.2).
//
// Starts the Rust engine on the bundled schemes (Resources/schemes): the
// selected scheme's table and metadata defaults come up, then the persisted
// preferences are applied. Only after that the IMKServer and the shared
// candidate panel are created, and the run loop spins. The process stays
// alive as an IMK server (LSUIElement: no Dock icon, but it can show the
// preferences window).

import Cocoa
import InputMethodKit

let connectionName = "Hime_1_Connection"
let bundleIdentifier = "com.hime.inputmethod.Hime"

// Shared candidate panel used by every HimeInputController instance.
var sharedCandidates: IMKCandidates!

guard let schemesDir = Bundle.main.path(forResource: "schemes", ofType: nil) else {
    fputs("Hime: schemes/ not found in bundle Resources\n", stderr)
    exit(1)
}
// Discovers the schemes, selects the persisted (or first) one, and hands the
// engine the persisted preferences (Preferences.swift); the candidate font
// size stays shell-side.
guard PreferencesModel.shared.startup(schemesDir: schemesDir) else {
    fputs("Hime: engine startup failed for the bundled schemes\n", stderr)
    exit(1)
}

// Reopening the app (open -a Hime) shows the preferences window. The
// delegate lives for the whole process, as NSApplication does not retain it.
let app = NSApplication.shared
let appDelegate = AppDelegate()
app.delegate = appDelegate

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

app.run()
