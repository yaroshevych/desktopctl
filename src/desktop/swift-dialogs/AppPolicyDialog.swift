import AppKit
import SwiftUI

private final class AppPolicyViewModel: ObservableObject {
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
}

private struct AppPolicyView: View {
    @ObservedObject var vm: AppPolicyViewModel
    let onClose: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            VStack(alignment: .leading, spacing: 14) {
                Text("Choose which frontmost apps DesktopCtl can control.")
                    .font(.system(size: 14))
                    .fixedSize(horizontal: false, vertical: true)

                Picker("", selection: $vm.policyMode) {
                    ForEach(PolicyMode.allCases, id: \.self) { mode in
                        Text(mode.title).tag(mode)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .frame(maxWidth: .infinity, alignment: .leading)

                VStack(alignment: .leading, spacing: 5) {
                    TextField("e.g. Safari, Slack, Terminal", text: $vm.appsCsv)
                        .disabled(vm.policyMode == .allowAll)
                        .textFieldStyle(.roundedBorder)
                        .frame(height: 24)

                    Text("Comma-separated app names. Example: Safari, Slack")
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                }

                VStack(alignment: .leading, spacing: 8) {
                    Toggle("Allow full-screen capture", isOn: $vm.allowFullScreenCapture)
                    Toggle("Allow clipboard access", isOn: $vm.clipboardAllowed)
                }
                .toggleStyle(.checkbox)

                Text(vm.warning)
                    .font(.system(size: 12))
                    .foregroundStyle(.orange)
                    .frame(height: 16, alignment: .leading)
            }
            .padding(.horizontal, 20)
            .padding(.top, 18)
            .padding(.bottom, 14)

            Divider()

            HStack {
                Spacer()
                Button("Close", action: onClose)
                    .keyboardShortcut(.defaultAction)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
        }
        .frame(width: 448)
    }
}

private final class AppPolicyWindowCloseCoordinator: NSObject, NSWindowDelegate {
    let onClose: () -> Void

    init(onClose: @escaping () -> Void) {
        self.onClose = onClose
    }

    func windowWillClose(_ notification: Notification) {
        onClose()
    }
}

private var _appPolicyCoordinator: AppPolicyWindowCloseCoordinator?

enum AppPolicyDialog {
    static func run(input: AppPolicyInput) {
        if let warning = input.warning, !warning.isEmpty {
            let alert = NSAlert()
            alert.messageText = "App Access Policy Config Error"
            alert.informativeText = "\(warning)\n\nDesktopCtl loaded default policy settings."
            alert.addButton(withTitle: "OK")
            alert.runModal()
        }

        let vm = AppPolicyViewModel(input)
        var didWrite = false

        func writeAndExit() {
            if didWrite {
                return
            }
            didWrite = true
            let output = AppPolicyOutput(
                saved: true,
                policyMode: vm.policyMode,
                apps: vm.apps,
                allowFullScreenCapture: vm.allowFullScreenCapture,
                clipboardAllowed: vm.clipboardAllowed
            )
            let encoder = JSONEncoder()
            encoder.keyEncodingStrategy = .convertToSnakeCase
            if let data = try? encoder.encode(output) {
                FileHandle.standardOutput.write(data)
            }
            NSApp.terminate(nil)
        }

        let view = AppPolicyView(vm: vm, onClose: writeAndExit)
        let hosting = NSHostingView(rootView: view)
        hosting.setFrameSize(hosting.fittingSize)

        let window = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 448, height: 300),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        window.title = "Agent Permissions"
        window.contentView = hosting
        window.isReleasedWhenClosed = false
        window.setContentSize(hosting.fittingSize)

        let coordinator = AppPolicyWindowCloseCoordinator(onClose: writeAndExit)
        _appPolicyCoordinator = coordinator
        window.delegate = coordinator

        NSApp.activate(ignoringOtherApps: true)
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.run()
    }
}
