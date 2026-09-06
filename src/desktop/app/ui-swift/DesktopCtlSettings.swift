import AppKit
import Carbon.HIToolbox
import SwiftUI

// MARK: - Models

struct DesktopCtlSettingsInput: Codable {
    var journal: JournalInput
    var appPolicy: AppPolicyInput
    var setupAccess: SetupAccessInput
    var launcher: LauncherInput
    var initialTab: String?
}

struct DesktopCtlSettingsOutput: Codable {
    var journal: JournalOutput
    var appPolicy: AppPolicyOutput
    var launcher: LauncherOutput
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
        // Query the daemon for permission state rather than calling OS APIs (like
        // CGPreflightScreenCaptureAccess) directly. On macOS 15+, calling those APIs from a
        // process that lacks screen recording permission triggers the system permission UI.
        DispatchQueue.global().async {
            guard let perms = DaemonIPC.checkPermissions() else { return }
            DispatchQueue.main.async {
                self.accessibilityGranted = perms.accessibility
                self.screenRecordingGranted = perms.screenRecording
            }
        }
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

private final class SettingsLauncherVM: ObservableObject {
    private static let defaultShortcut = LauncherShortcut(keyCode: kVK_Space, modifiers: 1 << 11)
    private static let saveQueue = DispatchQueue(label: "com.desktopctl.settings-save")

    @Published var renderKeyboardShortcuts: Bool
    @Published var useNativeNotifications: Bool
    @Published var agent: String
    let agents: [LauncherAgentOption]
    @Published var openShortcut: LauncherShortcut
    @Published var isRecording = false

    private var monitor: Any?

    init(_ input: LauncherInput) {
        agents = input.agents
        agent = agents.contains { $0.key == input.agent } ? input.agent : "pi"
        renderKeyboardShortcuts = input.renderKeyboardShortcuts
        useNativeNotifications = input.useNativeNotifications
        openShortcut = input.openShortcut
    }

    deinit {
        stopRecording()
    }

    func buildOutput() -> LauncherOutput {
        LauncherOutput(
            saved: true,
            agent: agent,
            renderKeyboardShortcuts: renderKeyboardShortcuts,
            useNativeNotifications: useNativeNotifications,
            openShortcut: openShortcut
        )
    }

    func saveLive() {
        let value = renderKeyboardShortcuts
        let nativeValue = useNativeNotifications
        let selectedAgent = agent
        let shortcut = openShortcut
        DaemonIPC.notifyLauncherSettingsChanged(
            agent: selectedAgent,
            renderKeyboardShortcuts: value,
            useNativeNotifications: nativeValue,
            openShortcut: shortcut)
        Self.saveQueue.async {
            _ = DaemonIPC.updateLauncherSettings(
                agent: selectedAgent,
                renderKeyboardShortcuts: value,
                useNativeNotifications: nativeValue,
                openShortcut: shortcut)
        }
    }

    func beginRecording() {
        stopRecording()
        isRecording = true
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            guard let self else { return nil }
            let keyCode = Int(event.keyCode)
            let flags = event.modifierFlags.intersection([.command, .option, .control, .shift])
            if keyCode == kVK_Escape && flags.isEmpty {
                stopRecording()
                return nil
            }

            let modifiers = Self.carbonModifiers(from: flags)
            guard modifiers != 0 || Self.functionKeyNames[keyCode] != nil else { return nil }
            openShortcut = LauncherShortcut(keyCode: keyCode, modifiers: modifiers)
            stopRecording()
            saveLive()
            return nil
        }
    }

    func stopRecording() {
        if let monitor {
            NSEvent.removeMonitor(monitor)
            self.monitor = nil
        }
        isRecording = false
    }

    var canResetShortcut: Bool {
        openShortcut != Self.defaultShortcut
    }

    func resetShortcut() {
        stopRecording()
        openShortcut = Self.defaultShortcut
        saveLive()
    }

    var shortcutLabel: String {
        Self.modifierSymbols(from: openShortcut.modifiers) + Self.keyName(for: openShortcut.keyCode)
    }

    private static func carbonModifiers(from flags: NSEvent.ModifierFlags) -> Int {
        var modifiers = 0
        if flags.contains(.command) { modifiers |= 1 << 8 }
        if flags.contains(.shift) { modifiers |= 1 << 9 }
        if flags.contains(.option) { modifiers |= 1 << 11 }
        if flags.contains(.control) { modifiers |= 1 << 12 }
        return modifiers
    }

    private static func modifierSymbols(from modifiers: Int) -> String {
        var symbols = ""
        if modifiers & (1 << 12) != 0 { symbols += "⌃" }
        if modifiers & (1 << 11) != 0 { symbols += "⌥" }
        if modifiers & (1 << 9) != 0 { symbols += "⇧" }
        if modifiers & (1 << 8) != 0 { symbols += "⌘" }
        return symbols
    }

    private static func keyName(for keyCode: Int) -> String {
        if let special = specialKeyNames[keyCode] { return special }
        if let function = functionKeyNames[keyCode] { return function }
        return ansiKeyNames[keyCode] ?? "Key \(keyCode)"
    }

    private static let specialKeyNames: [Int: String] = [
        kVK_Space: "Space", kVK_Return: "↵", kVK_ANSI_KeypadEnter: "⌤",
        kVK_Tab: "⇥", kVK_Delete: "⌫", kVK_ForwardDelete: "⌦",
        kVK_Escape: "⎋", kVK_LeftArrow: "←", kVK_RightArrow: "→",
        kVK_UpArrow: "↑", kVK_DownArrow: "↓"
    ]

    private static let functionKeyNames: [Int: String] = [
        kVK_F1: "F1", kVK_F2: "F2", kVK_F3: "F3", kVK_F4: "F4", kVK_F5: "F5",
        kVK_F6: "F6", kVK_F7: "F7", kVK_F8: "F8", kVK_F9: "F9", kVK_F10: "F10",
        kVK_F11: "F11", kVK_F12: "F12", kVK_F13: "F13", kVK_F14: "F14", kVK_F15: "F15",
        kVK_F16: "F16", kVK_F17: "F17", kVK_F18: "F18", kVK_F19: "F19", kVK_F20: "F20"
    ]

    private static let ansiKeyNames: [Int: String] = [
        0: "A", 1: "S", 2: "D", 3: "F", 4: "H", 5: "G", 6: "Z", 7: "X",
        8: "C", 9: "V", 11: "B", 12: "Q", 13: "W", 14: "E", 15: "R",
        16: "Y", 17: "T", 18: "1", 19: "2", 20: "3", 21: "4", 22: "6",
        23: "5", 24: "=", 25: "9", 26: "7", 27: "-", 28: "8", 29: "0",
        30: "]", 31: "O", 32: "U", 33: "[", 34: "I", 35: "P", 37: "L",
        38: "J", 39: "'", 40: "K", 41: ";", 42: "\\", 43: ",", 44: "/",
        45: "N", 46: "M", 47: ".", 50: "`"
    ]
}

// MARK: - Tab content views

private struct LauncherShortcutRecorder: View {
    @ObservedObject var vm: SettingsLauncherVM

    var body: some View {
        Text(vm.isRecording ? "Listening…" : vm.shortcutLabel)
            .font(.system(.body, design: .rounded).monospaced())
            .foregroundStyle(vm.isRecording ? Color.accentColor : .primary)
            .frame(width: 132, height: 26)
            .background(
                RoundedRectangle(cornerRadius: 6)
                    .fill(Color.primary.opacity(vm.isRecording ? 0.12 : 0.07))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 6)
                    .stroke(vm.isRecording ? Color.accentColor : Color.secondary.opacity(0.35))
            )
            .contentShape(Rectangle())
            .onTapGesture { vm.beginRecording() }
            .help("Click, then type a keyboard shortcut")
            .accessibilityAddTraits(.isButton)
            .onDisappear { vm.stopRecording() }
    }
}

private struct LauncherTabContent: View {
    @ObservedObject var vm: SettingsLauncherVM

    var body: some View {
        Form {
            LabeledContent("App Launcher:") {
                HStack(spacing: 6) {
                    LauncherShortcutRecorder(vm: vm)
                    Button { vm.resetShortcut() } label: {
                        Image(systemName: "arrow.counterclockwise")
                    }
                    .buttonStyle(.borderless)
                    .controlSize(.small)
                    .disabled(!vm.canResetShortcut)
                    .help("Reset to Option–Space")
                }
            }

            Picker("Agent:", selection: $vm.agent) {
                ForEach(vm.agents, id: \.key) { agent in
                    Text(agent.label).tag(agent.key)
                }
            }
            .pickerStyle(.menu)

            Picker("Terminal:", selection: .constant("ghostty")) {
                Text("Ghostty").tag("ghostty")
            }
            .pickerStyle(.menu)

            Toggle("Render keyboard shortcuts", isOn: $vm.renderKeyboardShortcuts)
                .onChange(of: vm.renderKeyboardShortcuts) { _ in
                    vm.saveLive()
                }

            Toggle("Use native notifications", isOn: $vm.useNativeNotifications)
                .onChange(of: vm.useNativeNotifications) { _ in
                    vm.saveLive()
                }
        }
        .formStyle(.columns)
        .padding(.horizontal, 20)
        .padding(.top, 20)
    }
}

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
    @ObservedObject var launcherVM: SettingsLauncherVM
    @State var selectedTab: String

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 4) {
                SettingsTabButton(title: "Launcher",     icon: "command",          tag: "launcher",    selected: $selectedTab)
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
                case "launcher":    LauncherTabContent(vm: launcherVM)
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
            permissionsVM.refresh()
        }
        .onReceive(NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)) { _ in
            permissionsVM.refresh()
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
        let launcherVM = SettingsLauncherVM(input.launcher)
        var didWrite = false

        func writeAndExit() {
            guard !didWrite else { return }
            didWrite = true
            let output = DesktopCtlSettingsOutput(
                journal: journalVM.buildOutput(),
                appPolicy: policyVM.output,
                launcher: launcherVM.buildOutput()
            )
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
            launcherVM: launcherVM,
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
