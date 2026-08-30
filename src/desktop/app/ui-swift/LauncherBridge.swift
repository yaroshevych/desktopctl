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
    var recentTasks: [LauncherTask] = []
    var allTasks: [LauncherTask] = []
    var showAll = false
    var screen = "Launcher"
    var sessionID = ""
    var sessionTitle = ""
    var sessionStatus = ""
    var terminalAvailable = false
    var messages: [(user: Bool, text: String)] = []

    var tasks: [LauncherTask] {
        showAll ? allTasks : recentTasks
    }
}

private final class LauncherModel: ObservableObject {
    @Published private(set) var renderState = LauncherRenderState()
    @Published var prompt = ""
    @Published var focusGeneration = 0
    @Published var selectedTaskID: String?
    @Published var showActionsMenu = false
    @Published private(set) var isScrolling = false
    private var scrollGeneration = 0
    var callback: LauncherActionCallback?

    func applySnapshot(_ data: Data) {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return }

        var next = LauncherRenderState()
        next.showAll = renderState.showAll
        let rawScreen = root["screen"]
        if let value = rawScreen as? String {
            next.screen = value
        } else if let value = rawScreen as? [String: Any],
                  let session = value["Session"] as? [String: Any] {
            next.screen = "Session"
            next.showAll = false
            next.sessionID = session["id"] as? String ?? ""
            next.sessionTitle = session["title"] as? String ?? "Session"
            next.sessionStatus = session["status"] as? String ?? ""
            next.terminalAvailable = session["terminal_available"] as? Bool ?? false
            next.messages = (session["messages"] as? [[String: Any]] ?? []).compactMap { message in
                guard let text = message["text"] as? String else { return nil }
                return (message["user"] as? Bool ?? false, text)
            }
        }

        let parseTasks: ([[String: Any]]) -> [LauncherTask] = { rows in
            rows.compactMap { row in
                guard let id = row["id"] as? String else { return nil }
                return LauncherTask(
                    id: id,
                    title: row["title"] as? String ?? "Untitled task",
                    preview: row["preview"] as? String ?? "",
                    status: row["status"] as? String ?? "",
                    unread: row["unread"] as? Bool ?? false
                )
            }
        }
        let recentRows = (root["recent"] as? [[String: Any]]) ?? []
        next.recentTasks = parseTasks(recentRows)
        next.allTasks = parseTasks((root["all"] as? [[String: Any]]) ?? recentRows)
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
        guard renderState.screen != "Session" else { return }

        // The controller intentionally keeps older sessions out of the initial
        // list. Reveal the full history as keyboard navigation lands on the last
        // recent row, while preserving that row as the selection.
        if delta > 0, !renderState.showAll, !renderState.allTasks.isEmpty {
            if renderState.recentTasks.isEmpty, selectedTaskID == nil {
                expandHistory(selecting: renderState.allTasks[0].id)
                return
            }
            if selectedTaskID == nil,
               renderState.recentTasks.count == 1,
               renderState.allTasks.count > renderState.recentTasks.count {
                expandHistory(selecting: renderState.recentTasks[0].id)
                return
            }
            if let selectedTaskID,
               let current = renderState.recentTasks.firstIndex(where: { $0.id == selectedTaskID }),
               current + 1 == renderState.recentTasks.count - 1,
               renderState.allTasks.count > renderState.recentTasks.count {
                expandHistory(selecting: renderState.recentTasks[current + 1].id)
                return
            }
            if let selectedTaskID,
               let current = renderState.recentTasks.firstIndex(where: { $0.id == selectedTaskID }),
               current == renderState.recentTasks.count - 1,
               renderState.allTasks.count > renderState.recentTasks.count {
                expandHistory(selecting: renderState.allTasks[renderState.recentTasks.count].id)
                return
            }
        }

        guard !renderState.tasks.isEmpty else { return }
        guard let selectedTaskID,
              let current = renderState.tasks.firstIndex(where: { $0.id == selectedTaskID })
        else {
            if delta > 0 {
                self.selectedTaskID = renderState.tasks[0].id
            }
            return
        }
        let updateSelection = {
            let next = current + delta
            if next < 0 {
                self.selectedTaskID = nil
                self.focusGeneration += 1
            } else {
                self.selectedTaskID = self.renderState.tasks[
                    min(next, self.renderState.tasks.count - 1)
                ].id
            }
        }
        updateSelection()
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

    func toggleActionsMenu() {
        withAnimation(.easeOut(duration: 0.16)) {
            showActionsMenu.toggle()
        }
    }

    func expandAllHistory() {
        guard !renderState.showAll else { return }
        emit(["type": "expand_history"])
        DispatchQueue.main.async {
            withAnimation(.easeInOut(duration: 0.24)) {
                self.renderState.showAll = true
            }
        }
    }

    func noteScrollWheel() {
        scrollGeneration += 1
        let generation = scrollGeneration
        isScrolling = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.18) {
            guard self.scrollGeneration == generation else { return }
            self.isScrolling = false
        }
    }

    func dismissActionsMenu() -> Bool {
        guard showActionsMenu else { return false }
        withAnimation(.easeOut(duration: 0.12)) {
            showActionsMenu = false
        }
        return true
    }

    func activateActionsMenu() -> Bool {
        guard showActionsMenu else { return false }
        showActionsMenu = false
        emit(["type": "open_settings"])
        return true
    }

    func prepareForPresentation() {
        renderState.showAll = false
        selectedTaskID = nil
        showActionsMenu = false
        focusPrompt()
    }

    func focusPrompt() {
        focusGeneration += 1
    }

    private func expandHistory(selecting taskID: String) {
        emit(["type": "expand_history"])
        // Let AppKit begin growing the panel before SwiftUI inserts rows that
        // do not fit in the current viewport.
        DispatchQueue.main.async {
            withAnimation(.easeInOut(duration: 0.24)) {
                self.renderState.showAll = true
            }
            self.selectedTaskID = taskID
        }
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
    @State private var hoveredTaskID: String?
    @State private var actionsButtonHovered = false
    @State private var showAllHovered = false
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if model.renderState.screen == "Session" {
                VStack(alignment: .leading, spacing: 12) {
                    sessionBody
                }
                .padding(LauncherTheme.Spacing.xxl)
            } else {
                launcherBody
            }
        }
        .background {
            ZStack {
                LauncherVisualEffectView()
                LauncherTheme.panelScrim(
                    colorScheme: colorScheme,
                    reduceTransparency: reduceTransparency
                )
            }
        }
        .clipShape(
            RoundedRectangle(
                cornerRadius: LauncherTheme.Radius.panel,
                style: .continuous
            )
        )
        .overlay {
            RoundedRectangle(
                cornerRadius: LauncherTheme.Radius.panel,
                style: .continuous
            )
            .strokeBorder(
                LauncherTheme.panelEdge(colorScheme: colorScheme),
                lineWidth: 0.5
            )
        }
        .overlay {
            if model.renderState.screen != "Session", model.showActionsMenu {
                Color.clear
                    .contentShape(Rectangle())
                    .onTapGesture { _ = model.dismissActionsMenu() }
            }
        }
        .overlay(alignment: .bottomTrailing) {
            if model.renderState.screen != "Session", model.showActionsMenu {
                actionsMenu
                    .padding(.trailing, LauncherTheme.Spacing.lg)
                    .padding(.bottom, 46)
                    .transition(
                        .opacity.combined(
                            with: .scale(scale: 0.94, anchor: .bottomTrailing)
                        )
                    )
            }
        }
        .overlay(alignment: .bottomTrailing) {
            if model.renderState.screen != "Session", !model.renderState.tasks.isEmpty {
                actionsButton
                    .padding(LauncherTheme.Spacing.lg)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onAppear {
            DispatchQueue.main.async { promptFocused = true }
        }
        .onChange(of: model.focusGeneration) { _ in
            promptFocused = false
            DispatchQueue.main.async {
                promptFocused = true
            }
        }
        .onChange(of: model.showActionsMenu) { visible in
            if visible {
                promptFocused = false
            } else {
                DispatchQueue.main.async { promptFocused = true }
            }
        }
        .onChange(of: model.isScrolling) { scrolling in
            if scrolling {
                hoveredTaskID = nil
                showAllHovered = false
            }
        }
    }

    @ViewBuilder
    private var launcherBody: some View {
        TextField("Ask DesktopCtl…", text: $model.prompt)
            .textFieldStyle(.plain)
            .font(.system(size: 20, weight: .regular, design: .rounded))
            .focused($promptFocused)
            .onSubmit { model.sendPrompt() }
            .frame(height: 50)
            .offset(y: 2)
            .padding(.horizontal, LauncherTheme.Spacing.xxl)
            .overlay(alignment: .bottom) {
                if !model.renderState.tasks.isEmpty {
                    Rectangle()
                        .fill(LauncherTheme.textTertiary.opacity(0.24))
                        .frame(height: 0.5)
                }
            }
            .accessibilityLabel("Launcher prompt")

        if !model.renderState.tasks.isEmpty {
            ScrollViewReader { proxy in
                ScrollView(.vertical, showsIndicators: false) {
                    ZStack(alignment: .topLeading) {
                        if let selectedTaskID = model.selectedTaskID,
                           let selectedIndex = model.renderState.tasks.firstIndex(
                               where: { $0.id == selectedTaskID }
                           ) {
                            RoundedRectangle(
                                cornerRadius: LauncherTheme.Radius.row,
                                style: .continuous
                            )
                            .fill(
                                LauncherTheme.selection(
                                    colorScheme: colorScheme,
                                    reduceTransparency: reduceTransparency
                                )
                            )
                            .frame(maxWidth: .infinity)
                            .frame(height: 42)
                            .padding(.horizontal, LauncherTheme.Spacing.md)
                            .offset(y: LauncherTheme.Spacing.md + CGFloat(selectedIndex) * 44)
                        }

                        LazyVStack(alignment: .leading, spacing: 2) {
                            ForEach(model.renderState.tasks) { task in
                                Button(action: { model.open(task) }) {
                                    taskRow(task)
                                }
                                .frame(height: 42)
                                .buttonStyle(.plain)
                                .onHover { hovered in
                                    hoveredTaskID = !model.isScrolling && hovered ? task.id : nil
                                }
                                .background {
                                    if hoveredTaskID == task.id,
                                       model.selectedTaskID != task.id {
                                        RoundedRectangle(
                                            cornerRadius: LauncherTheme.Radius.row,
                                            style: .continuous
                                        )
                                        .fill(
                                            LauncherTheme.hover(
                                                colorScheme: colorScheme,
                                                reduceTransparency: reduceTransparency
                                            )
                                        )
                                    }
                                }
                                .transition(.opacity.combined(with: .move(edge: .top)))
                                .id(task.id)
                                .accessibilityLabel(
                                    statusLabel(task.status).isEmpty
                                        ? task.title
                                        : "\(task.title), \(statusLabel(task.status))"
                                )
                                .accessibilityHint("Open task")
                            }
                            if !model.renderState.showAll,
                               model.renderState.allTasks.count > model.renderState.recentTasks.count {
                                Button(action: model.expandAllHistory) {
                                    HStack(spacing: 9) {
                                        Image(systemName: "chevron.down")
                                            .font(.system(size: 10, weight: .semibold))
                                            .frame(width: 10)
                                        Text("Show all")
                                            .font(.body)
                                        Spacer(minLength: 0)
                                    }
                                    .padding(.horizontal, 8)
                                    .frame(height: 42)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .contentShape(Rectangle())
                                }
                                .buttonStyle(.plain)
                                .frame(height: 42)
                                .foregroundStyle(LauncherTheme.textSecondary)
                                .background {
                                    if showAllHovered {
                                        RoundedRectangle(
                                            cornerRadius: LauncherTheme.Radius.row,
                                            style: .continuous
                                        )
                                        .fill(
                                            LauncherTheme.hover(
                                                colorScheme: colorScheme,
                                                reduceTransparency: reduceTransparency
                                            )
                                        )
                                    }
                                }
                                .onHover { hovered in
                                    showAllHovered = !model.isScrolling && hovered
                                }
                                .transition(.identity)
                                .accessibilityHint("Expand session history")
                            }
                        }
                        .padding(.horizontal, LauncherTheme.Spacing.md)
                        .padding(.top, LauncherTheme.Spacing.md)
                        .padding(.bottom, 50)
                        .animation(
                            .easeInOut(duration: 0.24),
                            value: model.renderState.showAll
                        )
                    }
                }
                .onChange(of: model.selectedTaskID) { selected in
                    hoveredTaskID = nil
                    if let selected {
                        proxy.scrollTo(selected, anchor: .center)
                    }
                }
            }
        }

    }

    private var actionsButton: some View {
        Button(action: model.toggleActionsMenu) {
            HStack(spacing: LauncherTheme.Spacing.md) {
                Text("Options")
                    .font(.system(size: 13, weight: .regular))
                    .foregroundStyle(LauncherTheme.textSecondary)
                HStack(spacing: 2) {
                    LauncherKeyCap(title: "⌘")
                    LauncherKeyCap(title: "K")
                }
            }
            .padding(.horizontal, LauncherTheme.Spacing.lg)
            .frame(height: 28)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(
                        actionsButtonHovered
                            ? Color.primary.opacity(colorScheme == .dark ? 0.16 : 0.10)
                            : Color.clear
                    )
            )
            .overlay {
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .stroke(
                        Color.primary.opacity(actionsButtonHovered ? 0.18 : 0),
                        lineWidth: 0.5
                    )
            }
            .contentShape(RoundedRectangle(cornerRadius: 7, style: .continuous))
        }
        .buttonStyle(.plain)
        .padding(3)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(
                    colorScheme == .dark
                        ? Color.white.opacity(0.07)
                        : Color.black.opacity(0.045)
                )
        )
        .onHover { hovered in
            withAnimation(.easeOut(duration: 0.1)) {
                actionsButtonHovered = hovered
            }
        }
        .accessibilityLabel("Options")
        .accessibilityHint("Open launcher options")
    }

    private var actionsMenu: some View {
        VStack(spacing: 0) {
            Button(action: { _ = model.activateActionsMenu() }) {
                HStack(spacing: LauncherTheme.Spacing.lg) {
                    Image(systemName: "gearshape")
                        .frame(width: 18)
                        .accessibilityHidden(true)
                    Text("Settings")
                        .font(.system(size: 13, weight: .regular))
                        .foregroundStyle(LauncherTheme.textSecondary)
                    Spacer(minLength: LauncherTheme.Spacing.xxl)
                    HStack(spacing: 2) {
                        LauncherKeyCap(title: "⌘")
                        LauncherKeyCap(title: ",")
                    }
                }
                .padding(.horizontal, LauncherTheme.Spacing.xl)
                .frame(width: 218, height: 38)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(
                        LauncherTheme.selection(
                            colorScheme: colorScheme,
                            reduceTransparency: reduceTransparency
                        )
                    )
            )
            .accessibilityLabel("Open Settings")
        }
        .padding(4)
        .background(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color(nsColor: .windowBackgroundColor))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(LauncherTheme.panelEdge(colorScheme: colorScheme), lineWidth: 0.5)
        )
        .shadow(color: .black.opacity(0.24), radius: 12, y: 5)
    }

    @ViewBuilder
    private var sessionBody: some View {
        HStack(spacing: 8) {
            LauncherBarButton(title: "Sessions", systemImage: "chevron.left", action: model.back)
                .accessibilityLabel("Back to sessions")
            Spacer()
            if model.renderState.terminalAvailable {
                LauncherBarButton(title: "Open in Ghostty", systemImage: "terminal", action: model.openInGhostty)
                    .accessibilityHint("Open this session in Ghostty")
            }
        }

        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    ForEach(Array(model.renderState.messages.enumerated()), id: \.offset) { index, message in
                        HStack {
                            if message.user { Spacer(minLength: 42) }
                            Text(message.text)
                                .textSelection(.enabled)
                                .padding(.horizontal, 13)
                                .padding(.vertical, 9)
                                .foregroundColor(message.user ? .white : .primary)
                                .background(
                                    RoundedRectangle(cornerRadius: 17, style: .continuous)
                                        .fill(
                                            message.user
                                                ? Color(nsColor: .systemBlue)
                                                : Color.primary.opacity(0.10)
                                        )
                                )
                            if !message.user { Spacer(minLength: 42) }
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
                LauncherBarButton(title: "Stop", systemImage: "stop.fill", action: model.cancelSession)
                    .keyboardShortcut(.cancelAction)
                    .accessibilityHint("Cancel running session")
            } else {
                HStack(spacing: 10) {
                    Image(systemName: "arrow.turn.down.left")
                        .font(.system(size: 14, weight: .medium))
                        .frame(width: 20, height: 22)
                        .foregroundColor(.secondary)
                        .accessibilityHidden(true)
                    TextField("Follow up…", text: $model.prompt)
                        .textFieldStyle(.plain)
                        .font(.system(size: 18, weight: .regular, design: .rounded))
                        .focused($promptFocused)
                        .onSubmit { model.sendPrompt() }
                        .accessibilityLabel("Follow-up prompt")
                    LauncherKeyCap(title: "↵")
                        .accessibilityLabel("Return to submit")
                }
                .padding(.horizontal, 4)
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
                    if !statusLabel(task.status).isEmpty {
                        Text(statusLabel(task.status))
                            .font(.caption2)
                            .foregroundColor(.secondary)
                    }
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
        case "Completed": return ""
        case "Failed": return "Failed"
        case "Cancelled": return "Cancelled"
        default: return value
        }
    }
}

private var model: LauncherModel?
private var hosting: NSHostingView<LauncherRootView>?
private var scrollWheelMonitor: Any?

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
    scrollWheelMonitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { event in
        if event.window === nextHosting.window, event.scrollingDeltaY != 0 {
            nextModel.noteScrollWheel()
        }
        return event
    }
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
    if let monitor = scrollWheelMonitor {
        NSEvent.removeMonitor(monitor)
        scrollWheelMonitor = nil
    }
    hosting?.removeFromSuperview()
    hosting = nil
    model = nil
}

@_cdecl("desktopctl_launcher_focus_prompt")
public func desktopctl_launcher_focus_prompt() {
    guard Thread.isMainThread else { return }
    model?.focusPrompt()
}

@_cdecl("desktopctl_launcher_prepare_for_presentation")
public func desktopctl_launcher_prepare_for_presentation() {
    guard Thread.isMainThread else { return }
    model?.prepareForPresentation()
}

@_cdecl("desktopctl_launcher_move_selection")
public func desktopctl_launcher_move_selection(_ delta: Int) {
    guard Thread.isMainThread else { return }
    model?.moveSelection(delta)
}

@_cdecl("desktopctl_launcher_toggle_actions_menu")
public func desktopctl_launcher_toggle_actions_menu() {
    guard Thread.isMainThread else { return }
    model?.toggleActionsMenu()
}

@_cdecl("desktopctl_launcher_dismiss_actions_menu")
public func desktopctl_launcher_dismiss_actions_menu() -> Bool {
    guard Thread.isMainThread else { return false }
    return model?.dismissActionsMenu() ?? false
}

@_cdecl("desktopctl_launcher_activate_actions_menu")
public func desktopctl_launcher_activate_actions_menu() -> Bool {
    guard Thread.isMainThread else { return false }
    return model?.activateActionsMenu() ?? false
}

@_cdecl("desktopctl_launcher_actions_menu_handles_navigation")
public func desktopctl_launcher_actions_menu_handles_navigation() -> Bool {
    guard Thread.isMainThread else { return false }
    return model?.showActionsMenu ?? false
}
