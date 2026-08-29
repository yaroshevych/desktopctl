import AppKit
import Foundation

let args = CommandLine.arguments
guard args.count >= 2 else {
    fputs("usage: desktopctl-dialogs <dialog-type>\n", stderr)
    exit(1)
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)

let stdinData = FileHandle.standardInput.readDataToEndOfFile()

let decoder = JSONDecoder()
decoder.keyDecodingStrategy = .convertFromSnakeCase

switch args[1] {
case "journal":
    guard let input = try? decoder.decode(JournalInput.self, from: stdinData) else {
        fputs("desktopctl-dialogs: failed to decode journal input\n", stderr)
        exit(1)
    }
    JournalDialog.run(input: input)
case "app-policy":
    guard let input = try? decoder.decode(AppPolicyInput.self, from: stdinData) else {
        fputs("desktopctl-dialogs: failed to decode app policy input\n", stderr)
        exit(1)
    }
    AppPolicyDialog.run(input: input)
case "setup-access":
    guard let input = try? decoder.decode(SetupAccessInput.self, from: stdinData) else {
        fputs("desktopctl-dialogs: failed to decode setup access input\n", stderr)
        exit(1)
    }
    SetupAccessDialog.run(input: input)
case "settings":
    guard let input = try? decoder.decode(DesktopCtlSettingsInput.self, from: stdinData) else {
        fputs("desktopctl-dialogs: failed to decode settings input\n", stderr)
        exit(1)
    }
    DesktopCtlSettings.run(input: input)
default:
    fputs("desktopctl-dialogs: unknown dialog type: \(args[1])\n", stderr)
    exit(1)
}
