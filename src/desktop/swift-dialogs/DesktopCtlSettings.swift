import AppKit
import ApplicationServices
import SwiftUI

// MARK: - Models

struct DesktopCtlSettingsInput: Codable {
    var journal: JournalInput
    var appPolicy: AppPolicyInput
    var setupAccess: SetupAccessInput
    var initialTab: String?
}

struct DesktopCtlSettingsOutput: Codable {
    var journal: JournalOutput
    var appPolicy: AppPolicyOutput
}

// MARK: - View models

private final class SettingsJournalVM: ObservableObject {
    @Published var enabled: Bool
    @Published var intervalSeconds: String
    @Published var outputDir: String

    init(_ input: JournalInput) {
        enabled = input.enabled
        intervalSeconds = String(input.intervalSeconds)
        outputDir = input.outputDir
    }

    func buildOutput() -> JournalOutput {
        let seconds = max(1, Int(intervalSeconds.trimmingCharacters(in: .whitespaces)) ?? 30)
        return JournalOutput(saved: true, enabled: enabled, intervalSeconds: seconds, outputDir: outputDir)
    }

    func chooseDirectory() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            outputDir = url.path(percentEncoded: false)
        }
    }
}

private final class SettingsPolicyVM: ObservableObject {
    @Published var policyMode: PolicyMode
    @Published var appsCsv: String
    @Published var allowFullScreenCapture: Bool
    @Published var clipboardAllowed: Bool

    init(_ input: AppPolicyInput) {
        policyMode = input.policyMode
        appsCsv = input.apps.joined(separator: ", ")
        allowFullScreenCapture = input.allowFullScreenCapture
        clipboardAllowed = input.clipboardAllowed
    }

    var apps: [String] {
        var seen = Set<String>()
        return appsCsv
            .split(separator: ",")
            .map { $0.trimmingCharacters(in: .whitespacesAndNewlines) }
            .filter { !$0.isEmpty }
            .filter { seen.insert($0.lowercased()).inserted }
    }

    var warning: String {
        policyMode != .allowAll && apps.isEmpty ? "Add at least one app for this mode." : ""
    }

    var output: AppPolicyOutput {
        AppPolicyOutput(saved: true, policyMode: policyMode, apps: apps,
                        allowFullScreenCapture: allowFullScreenCapture, clipboardAllowed: clipboardAllowed)
    }
}

private final class SettingsPermissionsVM: ObservableObject {
    @Published var cliInstalled: Bool
    @Published var accessibilityGranted: Bool
    @Published var screenRecordingGranted: Bool

    private let cliSource: String?
    private let candidateCliDirs: [String]

    init(_ input: SetupAccessInput) {
        cliInstalled = input.cliInstalled
        accessibilityGranted = input.accessibilityGranted
        screenRecordingGranted = input.screenRecordingGranted
        cliSource = input.cliSource
        candidateCliDirs = input.candidateCliDirs
    }

    func refresh() {
        cliInstalled = Self.checkCli(candidateCliDirs: candidateCliDirs)
        accessibilityGranted = AXIsProcessTrusted()
        screenRecordingGranted = CGPreflightScreenCaptureAccess()
    }

    func installAgentTool() {
        guard let cliSource else { return }
        for dir in candidateCliDirs where installSymlink(source: cliSource, dir: dir) {
            refresh(); return
        }
        refresh()
    }

    private func installSymlink(source: String, dir: String) -> Bool {
        let fm = FileManager.default
        guard (try? fm.createDirectory(atPath: dir, withIntermediateDirectories: true)) != nil else { return false }
        let link = (dir as NSString).appendingPathComponent("desktopctl")
        if let dest = try? fm.destinationOfSymbolicLink(atPath: link), dest == source { return true }
        if fm.fileExists(atPath: link) {
            let vals = try? URL(fileURLWithPath: link).resourceValues(forKeys: [.isSymbolicLinkKey])
            guard vals?.isSymbolicLink == true, (try? fm.removeItem(atPath: link)) != nil else { return false }
        }
        return (try? fm.createSymbolicLink(atPath: link, withDestinationPath: source)) != nil
    }

    private static func checkCli(candidateCliDirs: [String]) -> Bool {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/which")
        p.arguments = ["desktopctl"]
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try? p.run(); p.waitUntilExit()
        if p.terminationStatus == 0 { return true }
        return candidateCliDirs.contains { FileManager.default.fileExists(atPath: ($0 as NSString).appendingPathComponent("desktopctl")) }
    }
}

// MARK: - Tab content views

private struct JournalTabContent: View {
    @ObservedObject var vm: SettingsJournalVM

    var body: some View {
        VStack(spacing: 0) {
            Text("Journal periodically captures the active window and saves a Markdown note to your directory. Save what you worked on across days — fully local, nothing sent to the cloud. Great for a personal LLM wiki, work diary, or using with AI assistants.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 20)
                .padding(.top, 20)
                .padding(.bottom, 20)

            Form {
                Toggle("Enable", isOn: $vm.enabled)

                LabeledContent("Save to:") {
                    HStack {
                        Text(vm.outputDir.isEmpty ? "Not set" : vm.outputDir)
                            .lineLimit(1).truncationMode(.middle)
                            .foregroundStyle(.secondary)
                        Button("Choose…") { vm.chooseDirectory() }.fixedSize()
                    }
                }
                .disabled(!vm.enabled)

                LabeledContent("Interval:") {
                    HStack {
                        TextField("", text: $vm.intervalSeconds)
                            .textFieldStyle(.roundedBorder)
                            .frame(width: 56)
                            .multilineTextAlignment(.trailing)
                        Text("seconds").foregroundStyle(.secondary)
                    }
                }
                .disabled(!vm.enabled)
            }
            .formStyle(.columns)
            .padding(.horizontal, 20)
            .padding(.top, 8)
        }
    }
}

private struct PolicyTabContent: View {
    @ObservedObject var vm: SettingsPolicyVM

    var body: some View {
        VStack(spacing: 0) {
            Text("Applications which DesktopCtl can control and journal. Restrict access, so AI agents and Journal never touch your banking, passwords, or similar apps — you stay in control of what they can reach.")
                .font(.callout)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 20)
                .padding(.top, 20)
                .padding(.bottom, 20)

            Form {
                Picker("Mode:", selection: $vm.policyMode) {
                    ForEach(PolicyMode.allCases, id: \.self) { mode in
                        Text(mode.title).tag(mode)
                    }
                }
                .pickerStyle(.menu)

                TextField("Applications:", text: $vm.appsCsv, prompt: Text("e.g. Safari, Slack, Terminal"))
                    .textFieldStyle(.roundedBorder)
                    .disabled(vm.policyMode == .allowAll)

                if !vm.warning.isEmpty {
                    LabeledContent("") {
                        Text(vm.warning).foregroundStyle(.orange)
                    }
                }

                Toggle("Allow full-screen capture", isOn: $vm.allowFullScreenCapture)
                Toggle("Allow clipboard access", isOn: $vm.clipboardAllowed)
            }
            .formStyle(.columns)
            .padding(.horizontal, 20)
            .padding(.top, 8)
        }
    }
}

private struct PermissionsTabContent: View {
    @ObservedObject var vm: SettingsPermissionsVM

    var body: some View {
        VStack(spacing: 0) {
        Text("macOS Permissions for DesktopCtl to see your screen and control apps. Install the agent tool so AI assistants can reach your Mac from the terminal — fully local, no data sent to the cloud. Grant permissions via System Settings.")
            .font(.callout)
            .foregroundStyle(.secondary)
            .frame(maxWidth: .infinity, alignment: .leading)
            .padding(.horizontal, 20)
            .padding(.top, 20)
            .padding(.bottom, 4)
        Form {
            Section {
                permRow(name: "Agent Tool", verb: "Install", granted: vm.cliInstalled,
                        grantedText: "Installed", notGrantedText: "Not Installed",
                        description: "Your AI agent uses this tool to see and control your desktop. Once installed, agents can open apps, click buttons, type text, and wait for results.",
                        action: vm.installAgentTool)
                permRow(name: "Accessibility", verb: "Grant", granted: vm.accessibilityGranted,
                        grantedText: "Granted", notGrantedText: "Not Granted",
                        description: "Lets agents read what's on screen and interact with it — buttons, inputs, menus, and more.\n\nNote: if DesktopCtl is already in the list of allowed apps, remove and add it again.",
                        action: { openURL("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"); vm.refresh() })
                permRow(name: "Screen Recording", verb: "Grant", granted: vm.screenRecordingGranted,
                        grantedText: "Granted", notGrantedText: "Not Granted",
                        description: "Lets agents see your screen so they can navigate apps visually. All processing happens on your Mac.\n\nNote: if DesktopCtl is already in the list of allowed apps, remove and add it again.",
                        action: { openURL("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"); vm.refresh() })
            }

        }
        .formStyle(.grouped)
        } // VStack
    }

    private func permRow(name: String, verb: String, granted: Bool,
                         grantedText: String, notGrantedText: String,
                         description: String, action: @escaping () -> Void) -> some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 5) {
                HStack(spacing: 6) {
                    Circle()
                        .fill(granted ? Color.green : Color.orange)
                        .frame(width: 8, height: 8)
                        .padding(.top, 3)
                    Text("\(name): \(granted ? grantedText : notGrantedText)")
                        .foregroundStyle(granted ? Color.green : Color.orange)
                        .fontWeight(.medium)
                }
                Text(description)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer()
            Button(verb, action: action).disabled(granted).fixedSize()
        }
    }

    private func openURL(_ value: String) {
        if let url = URL(string: value) { NSWorkspace.shared.open(url) }
    }
}

// MARK: - Tab picker button

private struct TabButtonStyle: ButtonStyle {
    let isSelected: Bool
    @State private var isHovered = false

    func makeBody(configuration: Configuration) -> some View {
        let highlighted = isSelected || configuration.isPressed
        configuration.label
            .foregroundStyle(highlighted ? Color.accentColor : Color.secondary)
            .background(
                RoundedRectangle(cornerRadius: 8)
                    .fill(highlighted
                          ? Color(NSColor.quaternaryLabelColor)
                          : isHovered ? Color(NSColor.quinaryLabel) : Color.clear)
            )
            .contentShape(Rectangle())
            .onHover { isHovered = $0 }
    }
}

private struct SettingsTabButton: View {
    let title: String
    let icon: String
    let tag: String
    @Binding var selected: String

    private var isSelected: Bool { selected == tag }

    var body: some View {
        Button { selected = tag } label: {
            VStack(spacing: 3) {
                Image(systemName: icon)
                    .font(.system(size: 20, weight: .regular))
                    .frame(height: 24)
                Text(title)
                    .font(.system(size: 11))
            }
            .frame(width: 72, height: 52)
        }
        .buttonStyle(TabButtonStyle(isSelected: isSelected))
    }
}

// MARK: - Root view

private struct DesktopCtlSettingsView: View {
    @ObservedObject var journalVM: SettingsJournalVM
    @ObservedObject var policyVM: SettingsPolicyVM
    @ObservedObject var permissionsVM: SettingsPermissionsVM
    @State var selectedTab: String

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 4) {
                SettingsTabButton(title: "Journal",     icon: "book",             tag: "journal",     selected: $selectedTab)
                SettingsTabButton(title: "Applications", icon: "macwindow",       tag: "policy",      selected: $selectedTab)
                SettingsTabButton(title: "Permissions", icon: "checkmark.shield", tag: "permissions", selected: $selectedTab)
            }
            .padding(.top, 12)
            .padding(.bottom, 8)
            .frame(maxWidth: .infinity)
            .background(Color(NSColor.windowBackgroundColor).ignoresSafeArea(edges: .top))

            Divider()

            Group {
                switch selectedTab {
                case "policy":      PolicyTabContent(vm: policyVM)
                case "permissions": PermissionsTabContent(vm: permissionsVM)
                default:            JournalTabContent(vm: journalVM)
                }
            }
            .animation(.none, value: selectedTab)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)

            HStack(spacing: 16) {
                HStack(spacing: 4) {
                    Text("Website:").foregroundStyle(.secondary)
                    linkButton("desktopctl.com", url: "https://desktopctl.com")
                }
                HStack(spacing: 4) {
                    Text("GitHub:").foregroundStyle(.secondary)
                    linkButton("desktopctl", url: "https://github.com/yaroshevych/desktopctl")
                }
            }
            .font(.callout)
            .padding(.vertical, 10)
        }
        .frame(width: 520, height: 530)
        .onAppear {
            Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { _ in
                permissionsVM.refresh()
            }
        }
    }

    private func linkButton(_ label: String, url: String) -> some View {
        Button(label) {
            if let u = URL(string: url) { NSWorkspace.shared.open(u) }
        }
        .buttonStyle(.plain)
        .foregroundStyle(Color.accentColor)
        .onHover { inside in
            if inside { NSCursor.pointingHand.push() } else { NSCursor.pop() }
        }
    }
}

// MARK: - Window runner

private final class SettingsCloseCoordinator: NSObject, NSWindowDelegate {
    let onClose: () -> Void
    init(onClose: @escaping () -> Void) { self.onClose = onClose }
    func windowWillClose(_ notification: Notification) { onClose() }
}

private var _settingsCoordinator: SettingsCloseCoordinator?

enum DesktopCtlSettings {
    static func run(input: DesktopCtlSettingsInput) {
        if let warning = input.appPolicy.warning, !warning.isEmpty {
            let alert = NSAlert()
            alert.messageText = "App Access Policy Config Error"
            alert.informativeText = "\(warning)\n\nDesktopCtl loaded default policy settings."
            alert.addButton(withTitle: "OK")
            alert.runModal()
        }

        let journalVM = SettingsJournalVM(input.journal)
        let policyVM = SettingsPolicyVM(input.appPolicy)
        let permissionsVM = SettingsPermissionsVM(input.setupAccess)
        var didWrite = false

        func writeAndExit() {
            guard !didWrite else { return }
            didWrite = true
            let output = DesktopCtlSettingsOutput(journal: journalVM.buildOutput(), appPolicy: policyVM.output)
            let encoder = JSONEncoder()
            encoder.keyEncodingStrategy = .convertToSnakeCase
            if let data = try? encoder.encode(output) {
                FileHandle.standardOutput.write(data)
            }
            NSApp.terminate(nil)
        }

        let view = DesktopCtlSettingsView(
            journalVM: journalVM,
            policyVM: policyVM,
            permissionsVM: permissionsVM,
            selectedTab: input.initialTab ?? "journal"
        )

        let hosting = NSHostingView(rootView: view)
        hosting.setFrameSize(hosting.fittingSize)

        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 530),
            styleMask: [.titled, .closable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = "DesktopCtl"
        window.titlebarAppearsTransparent = true
        window.titleVisibility = .hidden
        window.isMovableByWindowBackground = true
        window.contentView = hosting
        window.isReleasedWhenClosed = false
        window.setContentSize(hosting.fittingSize)

        let coordinator = SettingsCloseCoordinator(onClose: writeAndExit)
        _settingsCoordinator = coordinator
        window.delegate = coordinator

        NSApp.activate(ignoringOtherApps: true)
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.run()
    }
}
