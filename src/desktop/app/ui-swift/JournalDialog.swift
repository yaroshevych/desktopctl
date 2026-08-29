import AppKit
import SwiftUI

private final class JournalViewModel: ObservableObject {
    @Published var enabled: Bool
    @Published var intervalSeconds: String
    @Published var outputDir: String

    init(_ input: JournalInput) {
        enabled = input.enabled
        intervalSeconds = String(input.intervalSeconds)
        outputDir = input.outputDir
    }
}

private struct JournalView: View {
    @ObservedObject var vm: JournalViewModel
    let onSave: () -> Void
    let onCancel: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section {
                    Toggle("Capture active window journal", isOn: $vm.enabled)

                    HStack(spacing: 8) {
                        Text("Capture every:")
                        Spacer()
                        TextField("30", text: $vm.intervalSeconds)
                            .frame(width: 56)
                            .multilineTextAlignment(.trailing)
                        Text("seconds")
                            .foregroundStyle(.secondary)
                    }
                    .disabled(!vm.enabled)

                    HStack(spacing: 8) {
                        Text("Save to:")
                        Text(vm.outputDir.isEmpty ? "Not set" : vm.outputDir)
                            .lineLimit(1)
                            .truncationMode(.middle)
                            .foregroundStyle(.secondary)
                        Spacer()
                        Button("Choose…") { chooseDirectory() }
                            .fixedSize()
                    }
                    .disabled(!vm.enabled)
                }
            }
            .formStyle(.grouped)
            .scrollDisabled(true)

            Divider()

            HStack {
                Spacer()
                Button("Cancel", action: onCancel)
                    .keyboardShortcut(.cancelAction)
                Button("Save", action: onSave)
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 12)
        }
        .frame(width: 420)
    }

    private func chooseDirectory() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            vm.outputDir = url.path(percentEncoded: false)
        }
    }
}

private var _coordinator: WindowCloseCoordinator?

private final class WindowCloseCoordinator: NSObject, NSWindowDelegate {
    func windowWillClose(_ notification: Notification) {
        NSApp.terminate(nil)
    }
}

enum JournalDialog {
    static func run(input: JournalInput) {
        let vm = JournalViewModel(input)

        func writeAndExit(output: JournalOutput?) {
            if let output {
                let encoder = JSONEncoder()
                encoder.keyEncodingStrategy = .convertToSnakeCase
                if let data = try? encoder.encode(output) {
                    FileHandle.standardOutput.write(data)
                }
            }
            NSApp.terminate(nil)
        }

        let view = JournalView(
            vm: vm,
            onSave: {
                let seconds = Int(vm.intervalSeconds.trimmingCharacters(in: .whitespaces)) ?? 30
                writeAndExit(output: JournalOutput(
                    saved: true,
                    enabled: vm.enabled,
                    intervalSeconds: max(1, seconds),
                    outputDir: vm.outputDir
                ))
            },
            onCancel: { writeAndExit(output: nil) }
        )

        let hosting = NSHostingView(rootView: view)
        hosting.setFrameSize(hosting.fittingSize)

        let window = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 420, height: 300),
            styleMask: [.titled, .closable],
            backing: .buffered,
            defer: false
        )
        window.title = "Journal"
        window.contentView = hosting
        window.isReleasedWhenClosed = false
        window.setContentSize(hosting.fittingSize)

        let coordinator = WindowCloseCoordinator()
        _coordinator = coordinator
        window.delegate = coordinator

        NSApp.activate(ignoringOtherApps: true)
        window.center()
        window.makeKeyAndOrderFront(nil)
        NSApp.run()
    }
}
