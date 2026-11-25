// Hime input method — program entry point (SPEC_MACOS §1, §3.2).
//
// Starts the Rust engine on the bundled schemes (Resources/schemes) beside
// the user data folder (SPEC §5.2): the data folders are created, the
// persisted model override is replayed, then the selected scheme's table and
// metadata defaults come up and the persisted preferences are applied. Only
// after that the IMKServer is created and the run loop spins. The process
// stays alive as an IMK server (LSUIElement: no Dock icon, but it can show
// the preferences window).

import Cocoa
import InputMethodKit

let connectionName = "Hime_1_Connection"
let bundleIdentifier = "com.hime.inputmethod.Hime"

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

// Startup banner: identifies the running build and proves the os_log path
// works — the Caps Lock field diagnostics (HimeInputController) depend on it.
let buildVersion = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "?"
NSLog("Hime %@: engine up, IMK server %@", buildVersion, connectionName)

app.run()
