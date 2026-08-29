import AppKit
import Foundation
import SwiftUI

public typealias LauncherActionCallback = @convention(c) (
    UnsafePointer<CChar>?, Int
) -> Void

private struct LauncherTask: Identifiable {
    let id: String
    let title: String
    let preview: String
    let status: String
    let unread: Bool
}

private struct LauncherRenderState {
    var tasks: [LauncherTask] = []
    var screen = "Launcher"
    var sessionID = ""
    var sessionTitle = ""
    var sessionStatus = ""
    var terminalAvailable = false
    var messages: [(user: Bool, text: String)] = []
}

private final class LauncherModel: ObservableObject {
    @Published private(set) var renderState = LauncherRenderState()
    @Published var prompt = ""
    @Published var focusGeneration = 0
    @Published var selectedTaskID: String?
    var callback: LauncherActionCallback?

    func applySnapshot(_ data: Data) {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return }

        var next = LauncherRenderState()
        let rawScreen = root["screen"]
        if let value = rawScreen as? String {
            next.screen = value
        } else if let value = rawScreen as? [String: Any],
                  let session = value["Session"] as? [String: Any] {
            next.screen = "Session"
            next.sessionID = session["id"] as? String ?? ""
            next.sessionTitle = session["title"] as? String ?? "Session"
            next.sessionStatus = session["status"] as? String ?? ""
            next.terminalAvailable = session["terminal_available"] as? Bool ?? false
            next.messages = (session["messages"] as? [[String: Any]] ?? []).compactMap { message in
                guard let text = message["text"] as? String else { return nil }
                return (message["user"] as? Bool ?? false, text)
            }
        }

        let rows = (root["recent"] as? [[String: Any]]) ?? []
        next.tasks = rows.compactMap { row in
            guard let id = row["id"] as? String else { return nil }
            return LauncherTask(
                id: id,
                title: row["title"] as? String ?? "Untitled task",
                preview: row["preview"] as? String ?? "",
                status: row["status"] as? String ?? "",
                unread: row["unread"] as? Bool ?? false
            )
        }
        renderState = next
        if let selectedTaskID,
           !next.tasks.contains(where: { $0.id == selectedTaskID }) {
            self.selectedTaskID = nil
        }
    }

    func sendPrompt() {
        if renderState.screen != "Session",
           let selectedTaskID,
           let selected = renderState.tasks.first(where: { $0.id == selectedTaskID }) {
            open(selected)
            return
        }
        let value = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return }
        if renderState.screen == "Session", !renderState.sessionID.isEmpty {
            emit(["type": "follow_up", "session_id": renderState.sessionID, "prompt": value])
        } else {
            emit(["type": "new_request", "prompt": value])
        }
        prompt = ""
    }

    func open(_ task: LauncherTask) {
        selectedTaskID = task.id
        emit(["type": "open_session", "session_id": task.id])
    }

    func moveSelection(_ delta: Int) {
        guard renderState.screen != "Session", !renderState.tasks.isEmpty else { return }
        guard let selectedTaskID,
              let current = renderState.tasks.firstIndex(where: { $0.id == selectedTaskID })
        else {
            if delta > 0 {
                self.selectedTaskID = renderState.tasks[0].id
            }
            return
        }
        let next = current + delta
        if next < 0 {
            self.selectedTaskID = nil
            focusGeneration += 1
        } else {
            self.selectedTaskID = renderState.tasks[min(next, renderState.tasks.count - 1)].id
        }
    }

    func back() {
        emit(["type": "return_to_launcher"])
    }

    func cancelSession() {
        guard !renderState.sessionID.isEmpty else { return }
        emit(["type": "cancel_session", "session_id": renderState.sessionID])
    }

    func openInGhostty() {
        guard !renderState.sessionID.isEmpty else { return }
        emit(["type": "open_in_ghostty", "session_id": renderState.sessionID])
    }

    private func emit(_ object: [String: String]) {
        guard let callback,
              let data = try? JSONSerialization.data(withJSONObject: object)
        else { return }
        data.withUnsafeBytes { bytes in
            guard let base = bytes.baseAddress?.assumingMemoryBound(to: CChar.self) else { return }
            callback(base, data.count)
        }
    }
}

private struct LauncherRootView: View {
    @ObservedObject var model: LauncherModel
    @FocusState private var promptFocused: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "sparkle.magnifyingglass")
                    .foregroundColor(.accentColor)
                    .accessibilityHidden(true)
                Text("DesktopCtl")
                    .font(.headline)
                Spacer()
                Text(model.renderState.screen == "Session" ? model.renderState.sessionTitle : "Launcher")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(1)
            }

            if model.renderState.screen == "Session" {
                sessionBody
            } else {
                launcherBody
            }
        }
        .padding(18)
        .background(.regularMaterial)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onAppear {
            DispatchQueue.main.async { promptFocused = true }
        }
        .onChange(of: model.focusGeneration) { _ in
            promptFocused = true
        }
    }

    @ViewBuilder
    private var launcherBody: some View {
        HStack(spacing: 8) {
            TextField("Ask DesktopCtl…", text: $model.prompt)
                .textFieldStyle(.roundedBorder)
                .focused($promptFocused)
                .onSubmit { model.sendPrompt() }
                .accessibilityLabel("Launcher prompt")
            Button("Send", action: model.sendPrompt)
                .keyboardShortcut(.defaultAction)
                .accessibilityHint("Submit launcher prompt")
        }

        if !model.renderState.tasks.isEmpty {
            Divider()
            Text("Recent tasks")
                .font(.caption)
                .foregroundColor(.secondary)
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 2) {
                        ForEach(model.renderState.tasks) { task in
                            Button(action: { model.open(task) }) {
                                taskRow(task)
                            }
                            .buttonStyle(.plain)
                            .background(
                                RoundedRectangle(cornerRadius: 6)
                                    .fill(
                                        model.selectedTaskID == task.id
                                            ? Color.accentColor.opacity(0.16)
                                            : Color.clear
                                    )
                            )
                            .id(task.id)
                            .accessibilityLabel("\(task.title), \(statusLabel(task.status))")
                            .accessibilityHint("Open task")
                        }
                    }
                }
                .onChange(of: model.selectedTaskID) { selected in
                    if let selected {
                        proxy.scrollTo(selected, anchor: .center)
                    }
                }
            }
            .frame(maxHeight: 220)
        }
    }

    @ViewBuilder
    private var sessionBody: some View {
        HStack(spacing: 8) {
            Button("‹ Sessions", action: model.back)
                .buttonStyle(.plain)
                .accessibilityLabel("Back to sessions")
            Spacer()
            if model.renderState.terminalAvailable {
                Button("Open in Ghostty", action: model.openInGhostty)
                    .buttonStyle(.bordered)
                    .accessibilityHint("Open this session in Ghostty")
            }
        }

        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    ForEach(Array(model.renderState.messages.enumerated()), id: \.offset) { index, message in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(message.user ? "You" : "Pi")
                                .font(.caption)
                                .foregroundColor(.secondary)
                            Text(message.text)
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                        }
                        .id(index)
                        .accessibilityElement(children: .combine)
                        .accessibilityLabel("\(message.user ? "You" : "Pi"): \(message.text)")
                    }
                }
                .padding(.vertical, 4)
            }
            .frame(maxHeight: 190)
            .onChange(of: model.renderState.messages.count) { _ in
                if let last = model.renderState.messages.indices.last {
                    proxy.scrollTo(last, anchor: .bottom)
                }
            }
        }

        HStack(spacing: 8) {
            if model.renderState.sessionStatus == "Running" {
                ProgressView()
                    .controlSize(.small)
                Text("Pi is working…")
                    .font(.caption)
                    .foregroundColor(.secondary)
                Spacer()
                Button("Stop", action: model.cancelSession)
                    .buttonStyle(.bordered)
                    .keyboardShortcut(.cancelAction)
                    .accessibilityHint("Cancel running session")
            } else {
                TextField("Follow up…", text: $model.prompt)
                    .textFieldStyle(.roundedBorder)
                    .focused($promptFocused)
                    .onSubmit { model.sendPrompt() }
                    .accessibilityLabel("Follow-up prompt")
                Button("Send", action: model.sendPrompt)
                    .keyboardShortcut(.defaultAction)
            }
        }
    }

    private func taskRow(_ task: LauncherTask) -> some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: task.unread ? "circle.fill" : "circle")
                .font(.system(size: 7))
                .foregroundColor(task.unread ? .accentColor : .secondary)
                .frame(width: 10, height: 18)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 6) {
                    Text(task.title)
                        .font(.body)
                        .lineLimit(1)
                    Text(statusLabel(task.status))
                        .font(.caption2)
                        .foregroundColor(.secondary)
                }
                if !task.preview.isEmpty {
                    Text(task.preview)
                        .font(.caption)
                        .foregroundColor(.secondary)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }

    private func statusLabel(_ value: String) -> String {
        switch value {
        case "Running": return "Working"
        case "Completed": return "Complete"
        case "Failed": return "Failed"
        case "Cancelled": return "Cancelled"
        default: return value
        }
    }
}

private var model: LauncherModel?
private var hosting: NSHostingView<LauncherRootView>?

@_cdecl("desktopctl_launcher_mount")
public func desktopctl_launcher_mount(
    _ parent: UnsafeMutableRawPointer?,
    _ callback: LauncherActionCallback?
) -> Bool {
    guard Thread.isMainThread, let parent else { return false }
    let parentView = Unmanaged<NSView>.fromOpaque(parent).takeUnretainedValue()

    let nextModel = LauncherModel()
    nextModel.callback = callback
    let nextHosting = NSHostingView(rootView: LauncherRootView(model: nextModel))
    nextHosting.frame = parentView.bounds
    nextHosting.autoresizingMask = [.width, .height]
    parentView.addSubview(nextHosting)
    model = nextModel
    hosting = nextHosting
    return true
}

@_cdecl("desktopctl_launcher_set_snapshot")
public func desktopctl_launcher_set_snapshot(
    _ json: UnsafePointer<CChar>?,
    _ length: Int
) {
    guard Thread.isMainThread, let json, length >= 0 else { return }
    model?.applySnapshot(Data(bytes: json, count: length))
}

@_cdecl("desktopctl_launcher_unmount")
public func desktopctl_launcher_unmount() {
    guard Thread.isMainThread else { return }
    hosting?.removeFromSuperview()
    hosting = nil
    model = nil
}

@_cdecl("desktopctl_launcher_focus_prompt")
public func desktopctl_launcher_focus_prompt() {
    guard Thread.isMainThread else { return }
    model?.focusGeneration += 1
}

@_cdecl("desktopctl_launcher_move_selection")
public func desktopctl_launcher_move_selection(_ delta: Int) {
    guard Thread.isMainThread else { return }
    model?.moveSelection(delta)
}
