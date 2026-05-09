import AppKit
import ApplicationServices
import SwiftUI

private let websiteURL = "https://desktopctl.com"

private final class SetupAccessViewModel: ObservableObject {
    @Published var cliInstalled: Bool
    @Published var accessibilityGranted: Bool
    @Published var screenRecordingGranted: Bool

    let cliSource: String?
    let candidateCliDirs: [String]

    init(_ input: SetupAccessInput) {
        cliInstalled = input.cliInstalled
        accessibilityGranted = input.accessibilityGranted
        screenRecordingGranted = input.screenRecordingGranted
        cliSource = input.cliSource
        candidateCliDirs = input.candidateCliDirs
    }

    func refresh() {
        cliInstalled = Self.cliInPath(candidateCliDirs: candidateCliDirs)
        accessibilityGranted = AXIsProcessTrusted()
        screenRecordingGranted = CGPreflightScreenCaptureAccess()
    }

    func installAgentTool() {
        guard let cliSource else { return }
        for dir in candidateCliDirs {
            if installSymlink(source: cliSource, dir: dir) {
                refresh()
                return
            }
        }
        refresh()
    }

    private func installSymlink(source: String, dir: String) -> Bool {
        let fm = FileManager.default
        do {
            try fm.createDirectory(atPath: dir, withIntermediateDirectories: true)
        } catch {
            return false
        }

        let linkPath = (dir as NSString).appendingPathComponent("desktopctl")
        if let destination = try? fm.destinationOfSymbolicLink(atPath: linkPath), destination == source {
            ensureShellPathContains(dir: dir)
            return true
        }

        if fm.fileExists(atPath: linkPath) {
            do {
                let values = try URL(fileURLWithPath: linkPath).resourceValues(forKeys: [.isSymbolicLinkKey])
                guard values.isSymbolicLink == true else { return false }
                try fm.removeItem(atPath: linkPath)
            } catch {
                return false
            }
        }

        do {
            try fm.createSymbolicLink(atPath: linkPath, withDestinationPath: source)
            ensureShellPathContains(dir: dir)
            return true
        } catch {
            return false
        }
    }

    private func ensureShellPathContains(dir: String) {
        guard let home = ProcessInfo.processInfo.environment["HOME"] else { return }
        let exportLine: String
        if dir == "\(home)/.local/bin" {
            exportLine = "export PATH=\"$HOME/.local/bin:$PATH\""
        } else if dir == "\(home)/bin" {
            exportLine = "export PATH=\"$HOME/bin:$PATH\""
        } else {
            return
        }
        appendExportLine(file: "\(home)/.zprofile", exportLine: exportLine)
        appendExportLine(file: "\(home)/.zshrc", exportLine: exportLine)
    }

    private func appendExportLine(file: String, exportLine: String) {
        let existing = (try? String(contentsOfFile: file, encoding: .utf8)) ?? ""
        guard !existing.contains(exportLine) else { return }
        let prefix = existing.isEmpty || existing.hasSuffix("\n") ? "" : "\n"
        let next = existing + prefix + exportLine + "\n"
        try? next.write(toFile: file, atomically: true, encoding: .utf8)
    }

    private static func cliInPath(candidateCliDirs: [String]) -> Bool {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/which")
        process.arguments = ["desktopctl"]
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            process.waitUntilExit()
            if process.terminationStatus == 0 {
                return true
            }
        } catch {}

        return candidateCliDirs.contains { dir in
            FileManager.default.fileExists(atPath: (dir as NSString).appendingPathComponent("desktopctl"))
        }
    }
}

private struct SetupAccessView: View {
    @ObservedObject var vm: SetupAccessViewModel
    let onClose: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            VStack(spacing: 0) {
                VStack(spacing: 0) {
                    accessRow(
                        name: "Agent Tool",
                        verb: "Install",
                        granted: vm.cliInstalled,
                        grantedText: "Installed",
                        notGrantedText: "Not Installed",
                        explanation: "Your AI agent uses this tool to see and control your desktop. Once installed, agents can open apps, click buttons, type text, and wait for results - working through any application on your Mac without manual help.",
                        action: vm.installAgentTool
                    )

                    Divider()

                    accessRow(
                        name: "Accessibility",
                        verb: "Grant",
                        granted: vm.accessibilityGranted,
                        grantedText: "Granted",
                        notGrantedText: "Not Granted",
                        explanation: "Lets agents read what's on screen and interact with it - buttons, inputs, menus, and more. Without this, agents can see the screen but cannot understand or act on what's in it.\n\nNote: if DesktopCtl is already in the list of allowed apps, remove and add it again.",
                        action: {
                            openURL("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
                            vm.refresh()
                        }
                    )

                    Divider()

                    accessRow(
                        name: "Screen Recording",
                        verb: "Grant",
                        granted: vm.screenRecordingGranted,
                        grantedText: "Granted",
                        notGrantedText: "Not Granted",
                        explanation: "Lets agents see your screen so they can navigate apps visually. All processing happens on your Mac. Nothing is uploaded or sent to your AI provider unless you explicitly ask it to.\n\nNote: if DesktopCtl is already in the list of allowed apps, remove and add it again.",
                        action: {
                            openURL("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
                            vm.refresh()
                        }
                    )

                    Divider()

                    HStack(alignment: .center, spacing: 16) {
                        Text("Learn more about how DesktopCtl works, what agents can do with it, and how to get started.")
                            .font(.system(size: 12))
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)

                        Spacer(minLength: 16)

                        Button("Website") { openURL(websiteURL) }
                            .controlSize(.large)
                            .fixedSize()
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 12)
                }
                .background(Color(nsColor: .controlBackgroundColor))
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .overlay(
                    RoundedRectangle(cornerRadius: 8)
                        .stroke(Color(nsColor: .separatorColor), lineWidth: 1)
                )
            }
            .padding(.horizontal, 20)
            .padding(.top, 18)
            .padding(.bottom, 16)

            Divider()

            HStack {
                Spacer()
                Button("Close", action: onClose)
                    .keyboardShortcut(.defaultAction)
                    .controlSize(.large)
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
        }
        .frame(width: 520)
    }

    private func accessRow(
        name: String,
        verb: String,
        granted: Bool,
        grantedText: String,
        notGrantedText: String,
        explanation: String,
        action: @escaping () -> Void
    ) -> some View {
        HStack(alignment: .center, spacing: 16) {
            VStack(alignment: .leading, spacing: 4) {
                HStack(spacing: 6) {
                    Circle()
                        .fill(granted ? Color.green : Color.orange)
                        .frame(width: 8, height: 8)
                    Text("\(name): \(granted ? grantedText : notGrantedText)")
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(granted ? Color.green : Color.orange)
                }

                Text(explanation)
                    .font(.system(size: 12))
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Spacer(minLength: 16)

            Button(verb, action: action)
                .disabled(granted)
                .controlSize(.large)
                .fixedSize()
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 12)
    }

    private func openURL(_ value: String) {
        if let url = URL(string: value) {
            NSWorkspace.shared.open(url)
        }
    }
}

private final class SetupAccessWindowCloseCoordinator: NSObject, NSWindowDelegate {
    func windowWillClose(_ notification: Notification) {
        NSApp.terminate(nil)
    }
}

private var _setupAccessCoordinator: SetupAccessWindowCloseCoordinator?

enum SetupAccessDialog {
    static func run(input: SetupAccessInput) {
        let vm = SetupAccessViewModel(input)
        let view = SetupAccessView(vm: vm, onClose: { NSApp.terminate(nil) })
        let hosting = NSHostingView(rootView: view)
        hosting.setFrameSize(hosting.fittingSize)

        let window = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 380),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        window.title = "Permissions"
        window.contentView = hosting
        window.isReleasedWhenClosed = false
        window.setContentSize(hosting.fittingSize)

        let coordinator = SetupAccessWindowCloseCoordinator()
        _setupAccessCoordinator = coordinator
        window.delegate = coordinator

        Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in
            vm.refresh()
        }

        NSApp.activate(ignoringOtherApps: true)
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.run()
    }
}
