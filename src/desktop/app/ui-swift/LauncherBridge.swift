import AppKit
import Foundation
import SwiftUI
import UserNotifications

public typealias LauncherActionCallback = @convention(c) (
    UnsafePointer<CChar>?, Int
) -> Void
public typealias NotificationActionCallback = @convention(c) (
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
    var revision: UInt64 = 0
    var recentTasks: [LauncherTask] = []
    var allTasks: [LauncherTask] = []
    var showAll = false
    var screen = "Launcher"
    var activeApp: String?
    var renderKeyboardShortcuts = true
    var sessionID = ""
    var sessionTitle = ""
    var sessionStatus = ""
    var terminalAvailable = false
    var messages: [(user: Bool, text: String)] = []

    var tasks: [LauncherTask] {
        showAll ? allTasks : recentTasks
    }

    var showsAllHistory: Bool {
        !showAll && allTasks.count > recentTasks.count
    }

    var additionalTaskCount: Int {
        max(0, allTasks.count - recentTasks.count)
    }
}

private final class LauncherModel: ObservableObject {
    @Published private(set) var renderState = LauncherRenderState()
    @Published var prompt = ""
    @Published var focusGeneration = 0
    @Published var selectedTaskID: String?
    @Published var showAllFocused = false
    @Published var showActionsMenu = false
    @Published private(set) var actionsMenuFocusIndex: Int?
    @Published private(set) var actionsMenuFocusIsKeyboard = false
    @Published var shareWindowContext = true
    @Published private(set) var isScrolling = false
    @Published private(set) var queuedFollowUps: [String] = []
    @Published private(set) var flushingFollowUps: [String] = []
    private var scrollGeneration = 0
    private var preserveScrollForNextSelection = false
    private var queuedFollowUpsFlushPending = false
    private var followUpRequestPending = false
    private var snapshotParseGeneration: UInt64 = 0
    var callback: LauncherActionCallback?

    var displayedQueuedFollowUps: [String] {
        flushingFollowUps + queuedFollowUps
    }

    func applySnapshot(_ data: Data) {
        snapshotParseGeneration &+= 1
        let generation = snapshotParseGeneration
        let showAll = renderState.showAll
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            guard let next = Self.parseSnapshot(data, showAll: showAll) else { return }
            DispatchQueue.main.async {
                guard let self,
                      self.snapshotParseGeneration == generation,
                      next.revision > self.renderState.revision
                else { return }
                self.commitSnapshot(next)
            }
        }
    }

    private static func parseSnapshot(_ data: Data, showAll: Bool) -> LauncherRenderState? {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }

        var next = LauncherRenderState()
        next.revision = (root["revision"] as? NSNumber)?.uint64Value ?? 0
        next.showAll = showAll
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
        next.activeApp = root["active_app"] as? String
        next.renderKeyboardShortcuts = root["render_keyboard_shortcuts"] as? Bool ?? true

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
        return next
    }

    private func commitSnapshot(_ next: LauncherRenderState) {
        if next.sessionID != renderState.sessionID {
            queuedFollowUps.removeAll()
            flushingFollowUps.removeAll()
            queuedFollowUpsFlushPending = false
            followUpRequestPending = false
        }
        if next.sessionStatus == "Running" {
            // The controller accepted the request represented by the batch.
            flushingFollowUps.removeAll()
            queuedFollowUpsFlushPending = false
            followUpRequestPending = false
        } else if next.sessionStatus == "Failed" || next.sessionStatus == "Cancelled" {
            // Keep text recoverable if the controller could not start the batch.
            if !flushingFollowUps.isEmpty {
                queuedFollowUps.insert(contentsOf: flushingFollowUps, at: 0)
            }
            flushingFollowUps.removeAll()
            queuedFollowUpsFlushPending = false
            followUpRequestPending = false
        }
        renderState = next
        if let selectedTaskID,
           !next.tasks.contains(where: { $0.id == selectedTaskID }) {
            self.selectedTaskID = nil
        }
        if showAllFocused && !next.showsAllHistory {
            showAllFocused = false
        }
    }

    func sendPrompt() {
        if showAllFocused, !renderState.showAll {
            expandHistory(selecting: renderState.allTasks[renderState.recentTasks.count].id)
            return
        }
        if renderState.screen != "Session",
           let selectedTaskID,
           let selected = renderState.tasks.first(where: { $0.id == selectedTaskID }) {
            open(selected)
            return
        }
        let value = prompt.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { return }
        if renderState.screen == "Session", !renderState.sessionID.isEmpty {
            if renderState.sessionStatus == "Running"
                || queuedFollowUpsFlushPending
                || followUpRequestPending {
                queuedFollowUps.append(value)
            } else {
                followUpRequestPending = true
                emit([
                    "type": "follow_up",
                    "session_id": renderState.sessionID,
                    "prompt": value,
                    "share_context": shareWindowContext,
                ])
            }
        } else {
            emit([
                "type": "new_request",
                "prompt": value,
                "share_context": shareWindowContext,
            ])
        }
        prompt = ""
    }

    func flushQueuedFollowUps() {
        guard renderState.screen == "Session",
              !renderState.sessionID.isEmpty,
              renderState.sessionStatus != "Running",
              !queuedFollowUps.isEmpty,
              !queuedFollowUpsFlushPending
        else { return }

        queuedFollowUpsFlushPending = true
        followUpRequestPending = true
        flushingFollowUps = queuedFollowUps
        queuedFollowUps.removeAll()
        let combinedPrompt = flushingFollowUps.joined(separator: "\n\n")
        emit([
            "type": "follow_up",
            "session_id": renderState.sessionID,
            "prompt": combinedPrompt,
            "share_context": shareWindowContext,
        ])
    }

    func open(_ task: LauncherTask) {
        showAllFocused = false
        selectedTaskID = task.id
        emit(["type": "open_session", "session_id": task.id])
    }

    func moveSelection(_ delta: Int) {
        guard renderState.screen != "Session" else { return }

        if !renderState.showAll, renderState.showsAllHistory {
            if showAllFocused {
                if delta > 0 {
                    expandHistory(selecting: renderState.allTasks[renderState.recentTasks.count].id)
                } else if delta < 0 {
                    showAllFocused = false
                    if let last = renderState.tasks.last {
                        selectedTaskID = last.id
                    } else {
                        focusGeneration += 1
                    }
                }
                return
            }
            if renderState.tasks.isEmpty {
                if delta > 0 {
                    showAllFocused = true
                }
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
                self.showAllFocused = false
                self.focusGeneration += 1
            } else if next >= self.renderState.tasks.count,
                      delta > 0,
                      !self.renderState.showAll,
                      self.renderState.showsAllHistory {
                self.selectedTaskID = nil
                self.showAllFocused = true
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

    func toggleActionsMenuFromMouse() {
        toggleActionsMenu(selectingTopForKeyboard: false)
    }

    func toggleActionsMenuFromKeyboard() {
        toggleActionsMenu(selectingTopForKeyboard: true)
    }

    private func toggleActionsMenu(selectingTopForKeyboard: Bool) {
        let opening = !showActionsMenu
        withAnimation(.easeOut(duration: 0.16)) {
            showActionsMenu = opening
            actionsMenuFocusIndex = opening && selectingTopForKeyboard ? 0 : nil
            actionsMenuFocusIsKeyboard = opening && selectingTopForKeyboard
        }
    }

    func focusActionsMenuItem(_ index: Int, fromKeyboard: Bool) {
        guard showActionsMenu, index >= 0, index < actionsMenuItemCount else { return }
        actionsMenuFocusIndex = index
        actionsMenuFocusIsKeyboard = fromKeyboard
    }

    func clearMouseActionsMenuFocus(_ index: Int) {
        guard showActionsMenu,
              !actionsMenuFocusIsKeyboard,
              actionsMenuFocusIndex == index
        else { return }
        actionsMenuFocusIndex = nil
    }

    func moveActionsMenuFocus(delta: Int, escapeToPromptAtTop: Bool) -> Bool {
        guard showActionsMenu, actionsMenuItemCount > 0 else { return false }
        let current: Int
        if let actionsMenuFocusIndex {
            current = actionsMenuFocusIndex
        } else {
            current = delta < 0 ? 0 : actionsMenuItemCount - 1
        }
        if escapeToPromptAtTop, delta < 0, current == 0 {
            _ = dismissActionsMenu()
            focusPrompt()
            return true
        }
        actionsMenuFocusIsKeyboard = true
        actionsMenuFocusIndex = (current + delta + actionsMenuItemCount) % actionsMenuItemCount
        return true
    }

    private var actionsMenuItemCount: Int { 2 }

    func toggleShareWindowContext() {
        shareWindowContext.toggle()
    }

    func activateShareWindowContextShortcut() {
        focusActionsMenuItem(0, fromKeyboard: true)
        toggleShareWindowContext()
    }

    func expandAllHistory() {
        guard !renderState.showAll else { return }
        emit(["type": "expand_history"])
        DispatchQueue.main.async {
            // AppKit animates the panel resize. Keep the row-set change
            // immediate so SwiftUI does not animate the layout a second time.
            self.renderState.showAll = true
            self.showAllFocused = false
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
            actionsMenuFocusIndex = nil
            actionsMenuFocusIsKeyboard = false
        }
        return true
    }

    func activateActionsMenu() -> Bool {
        guard showActionsMenu else { return false }
        switch actionsMenuFocusIndex ?? 0 {
        case 0:
            toggleShareWindowContext()
        case 1:
            openSettings()
        default:
            return false
        }
        return true
    }

    func openSettings() {
        showActionsMenu = false
        actionsMenuFocusIndex = nil
        actionsMenuFocusIsKeyboard = false
        emit(["type": "open_settings"])
    }

    func prepareForPresentation() {
        renderState.showAll = false
        selectedTaskID = nil
        showAllFocused = false
        preserveScrollForNextSelection = false
        showActionsMenu = false
        actionsMenuFocusIndex = nil
        actionsMenuFocusIsKeyboard = false
        focusPrompt()
    }

    func focusPrompt() {
        focusGeneration += 1
    }

    func consumePreserveScrollForNextSelection() -> Bool {
        guard preserveScrollForNextSelection else { return false }
        preserveScrollForNextSelection = false
        return true
    }

    private func expandHistory(selecting taskID: String) {
        emit(["type": "expand_history"])
        // The native panel resize and this row-set update are coordinated on
        // the main queue; AppKit owns the visible expansion animation.
        DispatchQueue.main.async {
            // AppKit owns the expansion animation; changing the row set here
            // must not trigger a competing SwiftUI layout animation.
            self.renderState.showAll = true
            self.showAllFocused = false
            self.preserveScrollForNextSelection = true
            self.selectedTaskID = taskID
        }
    }

    private func emit(_ object: [String: Any]) {
        guard let callback,
              let data = try? JSONSerialization.data(withJSONObject: object)
        else { return }
        data.withUnsafeBytes { bytes in
            guard let base = bytes.baseAddress?.assumingMemoryBound(to: CChar.self) else { return }
            callback(base, data.count)
        }
    }
}

private struct SessionBubbleTail: Shape {
    let pointsRight: Bool

    func path(in rect: CGRect) -> Path {
        var path = Path()

        if pointsRight {
            path.move(to: CGPoint(x: rect.minX, y: rect.minY))
            path.addLine(to: CGPoint(x: rect.minX, y: rect.maxY))
            path.addLine(to: CGPoint(x: rect.maxX, y: rect.maxY))
        } else {
            path.move(to: CGPoint(x: rect.maxX, y: rect.minY))
            path.addLine(to: CGPoint(x: rect.maxX, y: rect.maxY))
            path.addLine(to: CGPoint(x: rect.minX, y: rect.maxY))
        }

        path.closeSubpath()
        return path
    }
}

// SwiftUI's plain TextField draws its placeholder with the NSTextFieldCell but
// typed text with the field editor (an NSTextView). The two use different line
// metrics, so text visibly jumps a couple of points when the field gains focus.
// Building the field ourselves with a single-line cell makes both draw
// identically, eliminating the jump.
private final class AppKitPromptTextField: NSTextField {
    var onCommandReturn: (() -> Void)?

    override func keyDown(with event: NSEvent) {
        let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let isCommandReturn = (event.keyCode == 36 || event.keyCode == 76) && modifiers == .command
        guard isCommandReturn else {
            super.keyDown(with: event)
            return
        }
        onCommandReturn?()
    }
}

private struct AppKitPromptField: NSViewRepresentable {
    let placeholder: String
    @Binding var text: String
    var isFocused: FocusState<Bool>.Binding
    let onSubmit: () -> Void
    let onCommandReturn: (() -> Void)?

    init(
        placeholder: String,
        text: Binding<String>,
        isFocused: FocusState<Bool>.Binding,
        onSubmit: @escaping () -> Void,
        onCommandReturn: (() -> Void)? = nil
    ) {
        self.placeholder = placeholder
        self._text = text
        self.isFocused = isFocused
        self.onSubmit = onSubmit
        self.onCommandReturn = onCommandReturn
    }

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeNSView(context: Context) -> AppKitPromptTextField {
        let field = AppKitPromptTextField()
        field.onCommandReturn = onCommandReturn
        field.placeholderString = placeholder
        let systemFont = NSFont.systemFont(ofSize: 20, weight: .regular)
        if let descriptor = systemFont.fontDescriptor.withDesign(.rounded),
           let roundedFont = NSFont(descriptor: descriptor, size: 20) {
            field.font = roundedFont
        } else {
            field.font = systemFont
        }
        field.isBezeled = false
        field.isBordered = false
        field.drawsBackground = false
        field.isEditable = true
        field.isSelectable = true
        field.focusRingType = .none
        field.alignment = .natural
        field.cell?.usesSingleLineMode = true
        field.cell?.isScrollable = true
        field.cell?.wraps = false
        field.delegate = context.coordinator
        field.target = context.coordinator
        field.action = #selector(Coordinator.textChanged(_:))
        field.setAccessibilityLabel("Follow-up prompt")
        return field
    }

    func updateNSView(_ field: AppKitPromptTextField, context: Context) {
        let coordinator = context.coordinator
        coordinator.parent = self
        field.onCommandReturn = onCommandReturn

        if field.stringValue != text {
            field.stringValue = text
        }

        // Focus was requested (e.g. session opened) but hasn't landed yet:
        // keep asking until the field is in a window and becomes first
        // responder. Real focus changes are reported via the delegate.
        if isFocused.wrappedValue, field.currentEditor() == nil {
            attemptFocus(field)
        }
    }

    private func attemptFocus(_ field: NSTextField) {
        DispatchQueue.main.async { [weak field] in
            guard let field else { return }
            if field.window != nil {
                field.window?.makeFirstResponder(field)
            } else if let coordinator = field.delegate as? Coordinator, coordinator.parent != nil {
                attemptFocus(field)
            }
        }
    }

    final class Coordinator: NSObject, NSTextFieldDelegate {
        var parent: AppKitPromptField?

        @objc func textChanged(_ sender: NSTextField) {
            parent?.text = sender.stringValue
            if sender.currentEditor() != nil {
                parent?.onSubmit()
            }
        }

        func control(
            _ control: NSControl,
            textView: NSTextView,
            doCommandBy commandSelector: Selector
        ) -> Bool {
            guard commandSelector == #selector(NSResponder.insertNewline(_:)) else {
                return false
            }
            guard !textView.hasMarkedText() else {
                return false
            }

            guard let event = NSApp.currentEvent else {
                parent?.onSubmit()
                return true
            }
            let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            let nonCommandModifiers = modifiers.subtracting([.command, .numericPad])
            guard nonCommandModifiers.isEmpty else {
                return false
            }

            if modifiers.contains(.command) {
                parent?.onCommandReturn?()
            } else {
                parent?.text = control.stringValue
                parent?.onSubmit()
            }
            return true
        }

        func controlTextDidBeginEditing(_ obj: Notification) {
            guard let parent else { return }
            if !parent.isFocused.wrappedValue {
                parent.isFocused.wrappedValue = true
            }
        }

        func controlTextDidChange(_ obj: Notification) {
            guard let field = obj.object as? NSTextField else { return }
            parent?.text = field.stringValue
        }

        func controlTextDidEndEditing(_ obj: Notification) {
            guard let parent else { return }
            if parent.isFocused.wrappedValue {
                parent.isFocused.wrappedValue = false
            }
        }
    }
}

private struct LauncherRootView: View {
    @ObservedObject var model: LauncherModel
    @FocusState private var promptFocused: Bool
    @State private var hoveredTaskID: String?
    @State private var showAllHovered = false
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if model.renderState.screen == "Session" {
                sessionBody
                    .padding(.leading, LauncherTheme.Spacing.lg)
                    .padding(.trailing, LauncherTheme.Spacing.md)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
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
        // Keep the transparent pixels in the rounded corners attached to the
        // panel for hit testing. Without this, clicks there reach the window
        // behind the borderless panel.
        .contentShape(Rectangle())
        // Keep native resize handles inside the panel's input region even
        // though the visual surface has rounded corners.
        .overlay(alignment: .bottomLeading) {
            Rectangle()
                .fill(Color.black.opacity(0.001))
                .frame(width: 32, height: 32)
                .contentShape(Rectangle())
        }
        .overlay(alignment: .bottomTrailing) {
            Rectangle()
                .fill(Color.black.opacity(0.001))
                .frame(width: 32, height: 32)
                .contentShape(Rectangle())
        }
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
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .onAppear {
            DispatchQueue.main.async { promptFocused = true }
        }
        .onChange(of: model.renderState.screen) { screen in
            guard screen == "Session", model.renderState.sessionStatus != "Running" else {
                return
            }
            DispatchQueue.main.async {
                promptFocused = true
            }
        }
        .onChange(of: model.renderState.sessionStatus) { status in
            guard model.renderState.screen == "Session", status != "Running" else {
                return
            }
            DispatchQueue.main.async {
                promptFocused = true
            }
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
        HStack(spacing: LauncherTheme.Spacing.sm) {
            TextField("Ask DesktopCtl…", text: $model.prompt)
                .textFieldStyle(.plain)
                .font(.system(size: 20, weight: .regular, design: .rounded))
                .focused($promptFocused)
                .onSubmit { model.sendPrompt() }
                .accessibilityLabel("Launcher prompt")
            actionsButton
        }
        .frame(height: 50)
        .padding(.leading, LauncherTheme.Spacing.xxl)
        .padding(.trailing, LauncherTheme.Spacing.md)
        .overlay(alignment: .bottom) {
            if !model.renderState.tasks.isEmpty || model.renderState.showsAllHistory {
                Rectangle()
                    .fill(LauncherTheme.textTertiary.opacity(0.24))
                    .frame(height: 0.5)
            }
        }

        if !model.renderState.tasks.isEmpty || model.renderState.showsAllHistory {
            ScrollViewReader { proxy in
                ScrollView(.vertical, showsIndicators: false) {
                    ZStack(alignment: .topLeading) {
                        LazyVStack(alignment: .leading, spacing: 2) {
                            ForEach(model.renderState.tasks) { task in
                                Button(action: { model.open(task) }) {
                                    taskRow(task)
                                }
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .frame(height: 42)
                                .buttonStyle(.plain)
                                .onHover { hovered in
                                    hoveredTaskID = !model.isScrolling && hovered ? task.id : nil
                                }
                                .background {
                                    if model.selectedTaskID == task.id || hoveredTaskID == task.id {
                                        rowHighlight(isSelected: model.selectedTaskID == task.id)
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
                            if model.renderState.showsAllHistory {
                                Button(action: model.expandAllHistory) {
                                    showAllRow
                                }
                                .buttonStyle(.plain)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .frame(height: 42, alignment: .topLeading)
                                .foregroundStyle(LauncherTheme.textSecondary)
                                .background {
                                    if model.showAllFocused || showAllHovered {
                                        rowHighlight(isSelected: model.showAllFocused)
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
                    }
                }
                .onChange(of: model.selectedTaskID) { selected in
                    hoveredTaskID = nil
                    if model.consumePreserveScrollForNextSelection() {
                        return
                    }
                    if let selected {
                        proxy.scrollTo(selected, anchor: .center)
                    }
                }
            }
        }

    }

    @ViewBuilder
    private func rowHighlight(isSelected: Bool) -> some View {
        RoundedRectangle(
            cornerRadius: LauncherTheme.Radius.row,
            style: .continuous
        )
        .fill(
            isSelected
                ? LauncherTheme.selection(
                    colorScheme: colorScheme,
                    reduceTransparency: reduceTransparency
                )
                : LauncherTheme.hover(
                    colorScheme: colorScheme,
                    reduceTransparency: reduceTransparency
                )
        )
    }

    private var showAllRow: some View {
        HStack(alignment: .top, spacing: 9) {
            Image(systemName: "chevron.down")
                .font(.system(size: 10, weight: .semibold))
                .foregroundColor(.secondary)
                .frame(width: 10, height: 18)
            VStack(alignment: .leading, spacing: 2) {
                Text("Show all")
                    .font(.body)
                Text("\(model.renderState.additionalTaskCount) more sessions")
                    .font(.caption)
                    .foregroundColor(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
    }

    private var actionsButton: some View {
        optionsButton(
            title: model.renderState.activeApp ?? "Agent Context",
            accessibilityHint: "Open launcher options"
        )
    }

    private var sessionActionsButton: some View {
        optionsButton(
            title: model.renderState.activeApp ?? "Agent Context",
            accessibilityHint: "Open session options"
        )
    }

    private func optionsButton(title: String, accessibilityHint: String) -> some View {
        LauncherPillButton(action: model.toggleActionsMenuFromMouse) {
            HStack(spacing: LauncherTheme.Spacing.md) {
                Image(systemName: "macwindow.badge.plus")
                    .font(.system(size: 12, weight: .regular))
                    .accessibilityHidden(true)
                Text(title)
                    .font(.system(size: 13, weight: .regular))
                    .foregroundStyle(LauncherTheme.textSecondary)
                    .lineLimit(1)
                if model.renderState.renderKeyboardShortcuts {
                    HStack(spacing: 2) {
                        LauncherKeyCap(title: "⌘")
                        LauncherKeyCap(title: "K")
                    }
                }
            }
        }
        .accessibilityLabel(title)
        .accessibilityHint(accessibilityHint)
        .background {
            LauncherActionMenuAnchor(model: model)
        }
    }

    @ViewBuilder
    private var sessionBody: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 0) {
                LauncherPillButton(action: model.back) {
                    HStack(spacing: LauncherTheme.Spacing.md) {
                        Image(systemName: "chevron.left")
                            .font(.system(size: 13, weight: .regular))
                        Text("Sessions")
                            .font(.system(size: 13, weight: .regular))
                            .foregroundStyle(LauncherTheme.textSecondary)
                        if model.renderState.renderKeyboardShortcuts {
                            LauncherKeyCap(title: "Esc", horizontalPadding: 3)
                        }
                    }
                    .foregroundStyle(LauncherTheme.textSecondary)
                }
                    .accessibilityLabel("Back to sessions")
                Spacer()
                if model.renderState.terminalAvailable {
                    LauncherPillButton(action: model.openInGhostty) {
                        HStack(spacing: LauncherTheme.Spacing.md) {
                            Text("Continue in Pi")
                                .font(.system(size: 13, weight: .regular))
                                .foregroundStyle(LauncherTheme.textSecondary)
                            if model.renderState.renderKeyboardShortcuts {
                                HStack(spacing: 2) {
                                    LauncherKeyCap(title: "⌘")
                                    LauncherKeyCap(title: "↵")
                                }
                            }
                        }
                        .foregroundStyle(LauncherTheme.textSecondary)
                    }
                        .keyboardShortcut(.return, modifiers: .command)
                        .accessibilityHint("Continue this session in Pi")
                }
            }
            .frame(height: 50)

            Rectangle()
                .fill(LauncherTheme.textTertiary.opacity(0.24))
                .frame(height: 0.5)
                .frame(maxWidth: .infinity)
                .padding(.horizontal, -LauncherTheme.Spacing.lg)
                .padding(.top, LauncherTheme.Spacing.md)

            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 12) {
                        ForEach(Array(model.renderState.messages.enumerated()), id: \.offset) { index, message in
                            let bubbleColor = message.user
                                ? Color(nsColor: .systemBlue)
                                : Color.primary
                            let bubbleOpacity = message.user ? 1.0 : 0.10
                            HStack {
                                if message.user { Spacer(minLength: 42) }
                                Text(message.text)
                                    .textSelection(.enabled)
                                    .frame(
                                        maxWidth: message.text.count > 180 ? 560 : nil,
                                        alignment: .leading
                                    )
                                    .fixedSize(horizontal: false, vertical: true)
                                    .padding(.horizontal, 13)
                                    .padding(.vertical, 9)
                                    .foregroundColor(message.user ? .white : .primary)
                                    .background {
                                        ZStack(alignment: message.user ? .bottomTrailing : .bottomLeading) {
                                            RoundedRectangle(cornerRadius: 17, style: .continuous)
                                                .fill(bubbleColor)
                                            SessionBubbleTail(pointsRight: message.user)
                                                .fill(bubbleColor)
                                                .frame(width: 25, height: 14)
                                                .offset(x: message.user ? 3 : -3, y: 0)
                                        }
                                        .compositingGroup()
                                        .opacity(bubbleOpacity)
                                    }
                                if !message.user { Spacer(minLength: 42) }
                            }
                            .frame(maxWidth: .infinity, alignment: message.user ? .trailing : .leading)
                            .padding(.leading, message.user ? 0 : LauncherTheme.Spacing.xs)
                            .padding(.trailing, message.user ? LauncherTheme.Spacing.xs : 0)
                            .id(index)
                            .accessibilityElement(children: .combine)
                            .accessibilityLabel("\(message.user ? "You" : "Pi"): \(message.text)")
                        }
                        if model.renderState.sessionStatus == "Running" {
                            let workingBubbleColor = Color.primary
                            HStack {
                                HStack(spacing: 8) {
                                    ProgressView()
                                        .controlSize(.small)
                                    Text("Pi is working…")
                                        .foregroundColor(.primary)
                                }
                                .padding(.horizontal, 13)
                                .padding(.vertical, 9)
                                .background {
                                    ZStack(alignment: .bottomLeading) {
                                        RoundedRectangle(cornerRadius: 17, style: .continuous)
                                            .fill(workingBubbleColor)
                                        SessionBubbleTail(pointsRight: false)
                                            .fill(workingBubbleColor)
                                            .frame(width: 25, height: 14)
                                            .offset(x: -3, y: 0)
                                        }
                                        .compositingGroup()
                                        .opacity(0.10)
                                }
                                Spacer(minLength: 42)
                            }
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.leading, LauncherTheme.Spacing.xs)
                            .id("working")
                            .accessibilityElement(children: .combine)
                            .accessibilityLabel("Pi is working")
                        }
                        ForEach(Array(model.displayedQueuedFollowUps.enumerated()), id: \.offset) { index, message in
                            HStack {
                                Spacer(minLength: 42)
                                Text(message)
                                    .textSelection(.enabled)
                                    .frame(
                                        maxWidth: message.count > 180 ? 560 : nil,
                                        alignment: .leading
                                    )
                                    .fixedSize(horizontal: false, vertical: true)
                                    .padding(.horizontal, 13)
                                    .padding(.vertical, 9)
                                    .foregroundColor(.white)
                                    .background {
                                        ZStack(alignment: .bottomTrailing) {
                                            RoundedRectangle(cornerRadius: 17, style: .continuous)
                                                .fill(Color(nsColor: .systemBlue))
                                            if index == model.displayedQueuedFollowUps.count - 1 {
                                                SessionBubbleTail(pointsRight: true)
                                                    .fill(Color(nsColor: .systemBlue))
                                                    .frame(width: 25, height: 14)
                                                    .offset(x: 3, y: 0)
                                            }
                                        }
                                        .compositingGroup()
                                    }
                            }
                            .frame(maxWidth: .infinity, alignment: .trailing)
                            .padding(.trailing, LauncherTheme.Spacing.xs)
                            .id("queued-\(index)")
                            .accessibilityElement(children: .combine)
                            .accessibilityLabel("You: \(message)")
                        }
                    }
                    .padding(.top, 4)
                    .padding(.bottom, 8)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .onAppear {
                    if let last = model.displayedQueuedFollowUps.indices.last {
                        DispatchQueue.main.async {
                            proxy.scrollTo("queued-\(last)", anchor: .bottom)
                        }
                        return
                    }
                    if model.renderState.sessionStatus == "Running" {
                        DispatchQueue.main.async {
                            proxy.scrollTo("working", anchor: .bottom)
                        }
                        return
                    }
                    guard let last = model.renderState.messages.indices.last else { return }
                    DispatchQueue.main.async {
                        proxy.scrollTo(last, anchor: .bottom)
                    }
                }
                .onChange(of: model.renderState.messages.count) { _ in
                    if let last = model.displayedQueuedFollowUps.indices.last {
                        DispatchQueue.main.async {
                            proxy.scrollTo("queued-\(last)", anchor: .bottom)
                        }
                        return
                    }
                    guard let last = model.renderState.messages.indices.last else { return }
                    DispatchQueue.main.async {
                        proxy.scrollTo(last, anchor: .bottom)
                    }
                }
                .onChange(of: model.displayedQueuedFollowUps.count) { _ in
                    guard let last = model.displayedQueuedFollowUps.indices.last else { return }
                    DispatchQueue.main.async {
                        proxy.scrollTo("queued-\(last)", anchor: .bottom)
                    }
                }
                .onChange(of: model.renderState.sessionStatus) { status in
                    if status == "Completed" {
                        model.flushQueuedFollowUps()
                        return
                    }
                    if status == "Running" {
                        DispatchQueue.main.async {
                            proxy.scrollTo("working", anchor: .bottom)
                        }
                    }
                }
            }

            Rectangle()
                .fill(LauncherTheme.textTertiary.opacity(0.24))
                .frame(height: 0.5)
                .frame(maxWidth: .infinity)
                .padding(.horizontal, -LauncherTheme.Spacing.lg)

            HStack(spacing: 8) {
                AppKitPromptField(
                    placeholder: "Follow up…",
                    text: $model.prompt,
                    isFocused: $promptFocused,
                    onSubmit: { model.sendPrompt() },
                    onCommandReturn: model.openInGhostty
                )
                .frame(maxWidth: .infinity)
                .frame(height: LauncherTheme.controlHeight)
                if model.renderState.sessionStatus == "Running" {
                    Spacer()
                    LauncherBarButton(title: "Stop", systemImage: "stop.fill", action: model.cancelSession)
                        .keyboardShortcut(.cancelAction)
                        .accessibilityHint("Cancel running session")
                }
                sessionActionsButton
            }
            .frame(maxWidth: .infinity)
            .frame(height: 50)
            .padding(.leading, 4)
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

private struct LauncherActionsMenu: View {
    @ObservedObject var model: LauncherModel
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    var body: some View {
        VStack(spacing: 0) {
            LauncherActionsMenuRow(
                model: model,
                index: 0,
                title: "Share window context",
                systemImage: model.shareWindowContext ? "checkmark.square.fill" : "square",
                showsIcon: true,
                systemImageColor: model.shareWindowContext
                    ? LauncherTheme.textSecondary
                    : LauncherTheme.textTertiary,
                accessibilityValue: model.shareWindowContext ? "On" : "Off",
                keyboardShortcut: model.renderState.renderKeyboardShortcuts ? ["S"] : nil,
                secondaryKeyboardShortcut: nil,
                action: model.toggleShareWindowContext
            )

            LauncherActionsMenuRow(
                model: model,
                index: 1,
                title: "Settings",
                systemImage: "gearshape",
                showsIcon: false,
                systemImageColor: LauncherTheme.textSecondary,
                accessibilityValue: "",
                keyboardShortcut: model.renderState.renderKeyboardShortcuts ? ["⌘", ","] : nil,
                secondaryKeyboardShortcut: nil,
                action: { _ = model.activateActionsMenu() }
            )
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
        .frame(width: 246, height: 90)
    }
}

private struct LauncherActionsMenuRow: View {
    @ObservedObject var model: LauncherModel
    let index: Int
    let title: String
    let systemImage: String
    let showsIcon: Bool
    let systemImageColor: Color
    let accessibilityValue: String
    let keyboardShortcut: [String]?
    let secondaryKeyboardShortcut: [String]?
    let action: () -> Void

    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @State private var isHovered = false

    var body: some View {
        Button(action: {
            model.focusActionsMenuItem(index, fromKeyboard: false)
            action()
        }) {
            HStack(spacing: LauncherTheme.Spacing.lg) {
                if showsIcon {
                    Image(systemName: systemImage)
                        .foregroundStyle(systemImageColor)
                        .frame(width: 18)
                        .accessibilityHidden(true)
                } else {
                    Color.clear
                        .frame(width: 18)
                        .accessibilityHidden(true)
                }
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: LauncherTheme.Spacing.sm) {
                        Text(title)
                            .font(.system(size: 13, weight: .regular))
                            .foregroundStyle(LauncherTheme.textSecondary)
                            .lineLimit(1)
                            .layoutPriority(1)
                        Spacer(minLength: LauncherTheme.Spacing.sm)
                        if let keyboardShortcut {
                            HStack(spacing: 2) {
                                ForEach(keyboardShortcut, id: \.self) { key in
                                    LauncherKeyCap(title: key)
                                }
                            }
                        }
                    }
                    if let secondaryKeyboardShortcut {
                        HStack(spacing: 2) {
                            Spacer(minLength: 0)
                            ForEach(secondaryKeyboardShortcut, id: \.self) { key in
                                LauncherKeyCap(title: key)
                            }
                        }
                    }
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(.horizontal, LauncherTheme.Spacing.xl)
            .frame(width: 238, height: 38)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(rowBackground)
        .onHover { hovered in
            isHovered = hovered
            if hovered {
                model.focusActionsMenuItem(index, fromKeyboard: false)
            } else {
                model.clearMouseActionsMenuFocus(index)
            }
        }
        .accessibilityLabel(title)
        .accessibilityValue(accessibilityValue)
    }

    private var rowBackground: some View {
        RoundedRectangle(cornerRadius: 7, style: .continuous)
            .fill(
                model.actionsMenuFocusIsKeyboard
                    ? model.actionsMenuFocusIndex == index
                        ? LauncherTheme.selection(
                            colorScheme: colorScheme,
                            reduceTransparency: reduceTransparency
                        )
                        : Color.clear
                    : isHovered
                        ? LauncherTheme.hover(
                            colorScheme: colorScheme,
                            reduceTransparency: reduceTransparency
                        )
                        : model.actionsMenuFocusIndex == index
                            ? LauncherTheme.selection(
                                colorScheme: colorScheme,
                                reduceTransparency: reduceTransparency
                            )
                            : Color.clear
            )
    }

}

private var model: LauncherModel?
private var hosting: NSHostingView<LauncherRootView>?
private var actionMenuPanel: NSPanel?
private weak var actionMenuParent: NSWindow?
private var scrollWheelMonitor: Any?
private var actionMenuEventMonitor: Any?
private var actionMenuKeyEventMonitor: Any?
private var actionMenuActivationObserver: NSObjectProtocol?
private var launcherSettingsObserver: NSObjectProtocol?

private let launcherActionsMenuSize = NSSize(width: 246, height: 90)

private final class LauncherActionMenuPanel: NSPanel {
    weak var menuModel: LauncherModel?

    override var canBecomeKey: Bool { true }
    override var canBecomeMain: Bool { false }

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if let menuModel, handleActionMenuKeyEvent(event, model: menuModel) {
            return true
        }
        return super.performKeyEquivalent(with: event)
    }

    override func keyDown(with event: NSEvent) {
        if let menuModel, handleActionMenuKeyEvent(event, model: menuModel) {
            return
        }
        super.keyDown(with: event)
    }

    override func sendEvent(_ event: NSEvent) {
        if event.type == .keyDown,
           let menuModel,
           handleActionMenuKeyEvent(event, model: menuModel)
        {
            return
        }
        super.sendEvent(event)
    }
}

private func handleActionMenuKeyEvent(_ event: NSEvent, model: LauncherModel) -> Bool {
    guard model.showActionsMenu else { return false }
    let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
    let characters = event.charactersIgnoringModifiers?.lowercased()
    if modifiers == .command, characters == "k" {
        model.toggleActionsMenuFromKeyboard()
        return true
    }
    if modifiers == .command, characters == "," {
        model.openSettings()
        return true
    }
    if modifiers == .command, [36, 76].contains(event.keyCode) {
        model.openInGhostty()
        return true
    }
    if modifiers.isEmpty, characters == "s" {
        model.activateShareWindowContextShortcut()
        return true
    }
    switch event.keyCode {
    case 53 where modifiers.isEmpty:
        _ = model.dismissActionsMenu()
        return true
    case 126:
        _ = model.moveActionsMenuFocus(delta: -1, escapeToPromptAtTop: true)
        return true
    case 125:
        _ = model.moveActionsMenuFocus(delta: 1, escapeToPromptAtTop: false)
        return true
    case 48 where modifiers == .shift || modifiers.isEmpty:
        let backwards = modifiers == .shift
        _ = model.moveActionsMenuFocus(
            delta: backwards ? -1 : 1,
            escapeToPromptAtTop: false
        )
        return true
    case 36 where modifiers.isEmpty, 76 where modifiers.isEmpty, 49 where modifiers.isEmpty:
        _ = model.activateActionsMenu()
        return true
    default:
        return false
    }
}

private func handleLauncherTabEvent(_ event: NSEvent, model: LauncherModel) -> Bool {
    guard !model.showActionsMenu,
          model.renderState.screen != "Session",
          event.keyCode == 48
    else { return false }
    let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
    guard modifiers.isEmpty || modifiers == .shift else { return false }
    model.moveSelection(modifiers == .shift ? -1 : 1)
    return true
}

private struct LauncherActionMenuAnchor: NSViewRepresentable {
    @ObservedObject var model: LauncherModel

    func makeNSView(context: Context) -> NSView {
        NSView(frame: .zero)
    }

    func updateNSView(_ view: NSView, context: Context) {
        DispatchQueue.main.async {
            guard model.showActionsMenu else {
                hideActionMenuPanel()
                return
            }
            showActionMenuPanel(for: model, anchoredTo: view)
        }
    }
}

private func showActionMenuPanel(for model: LauncherModel, anchoredTo anchor: NSView) {
    guard let window = anchor.window else { return }
    let panel: NSPanel
    if let actionMenuPanel {
        panel = actionMenuPanel
    } else {
        let nextPanel = LauncherActionMenuPanel(
            contentRect: NSRect(origin: .zero, size: launcherActionsMenuSize),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        nextPanel.isReleasedWhenClosed = false
        nextPanel.isFloatingPanel = true
        nextPanel.hidesOnDeactivate = false
        nextPanel.hasShadow = true
        nextPanel.backgroundColor = .clear
        nextPanel.isOpaque = false
        nextPanel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]

        let menuHosting = NSHostingView(rootView: LauncherActionsMenu(model: model))
        menuHosting.frame = NSRect(origin: .zero, size: launcherActionsMenuSize)
        menuHosting.autoresizingMask = [.width, .height]
        nextPanel.contentView = menuHosting
        nextPanel.menuModel = model
        actionMenuPanel = nextPanel
        panel = nextPanel
    }

    if actionMenuParent !== window {
        if let actionMenuParent {
            actionMenuParent.removeChildWindow(panel)
        }
        window.addChildWindow(panel, ordered: .above)
        actionMenuParent = window
    }

    let anchorInWindow = anchor.convert(anchor.bounds, to: nil)
    let anchorOnScreen = window.convertToScreen(anchorInWindow)
    let visibleFrame = window.screen?.visibleFrame ?? NSScreen.main?.visibleFrame
    let bounds = visibleFrame ?? NSRect(origin: .zero, size: launcherActionsMenuSize)
    var x = anchorOnScreen.maxX - launcherActionsMenuSize.width
    x = min(max(x, bounds.minX), bounds.maxX - launcherActionsMenuSize.width)
    let spacing: CGFloat = 8
    let belowY = anchorOnScreen.minY - launcherActionsMenuSize.height - spacing
    let aboveY = anchorOnScreen.maxY + spacing
    var y = belowY >= bounds.minY ? belowY : aboveY
    y = min(max(y, bounds.minY), bounds.maxY - launcherActionsMenuSize.height)

    panel.setFrame(
        NSRect(origin: NSPoint(x: x, y: y), size: launcherActionsMenuSize),
        display: true
    )
    panel.makeKeyAndOrderFront(nil)
}

private func hideActionMenuPanel() {
    guard let actionMenuPanel else { return }
    actionMenuPanel.orderOut(nil)
    if let actionMenuParent {
        actionMenuParent.removeChildWindow(actionMenuPanel)
        if actionMenuParent.isVisible {
            actionMenuParent.makeKeyAndOrderFront(nil)
        }
    }
    actionMenuParent = nil
}

private func refocusVisibleActionMenu() {
    guard let actionMenuPanel,
          actionMenuPanel.isVisible,
          model?.showActionsMenu == true
    else { return }
    actionMenuPanel.makeKeyAndOrderFront(nil)
}

private final class DesktopCtlNotificationDelegate: NSObject, UNUserNotificationCenterDelegate {
    var callback: NotificationActionCallback?

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        if response.actionIdentifier == UNNotificationDefaultActionIdentifier,
           let sessionID = response.notification.request.content.userInfo["session_id"] as? String,
           !sessionID.isEmpty {
            sessionID.withCString { pointer in
                callback?(pointer, sessionID.utf8.count)
            }
        }
        completionHandler()
    }

    func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler:
            @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }
}

private let notificationDelegate = DesktopCtlNotificationDelegate()

private func postCompletionNotification(title: String, body: String, sessionID: String) {
    let center = UNUserNotificationCenter.current()

    let post = {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        if !sessionID.isEmpty {
            content.userInfo = ["session_id": sessionID]
        }
        content.sound = .default

        let request = UNNotificationRequest(
            identifier: "completion-\(UUID().uuidString)",
            content: content,
            trigger: nil
        )
        center.add(request) { error in
            if let error {
                fputs("desktopctl: failed to post completion notification: \(error)\n", stderr)
            }
        }
    }

    center.getNotificationSettings { settings in
        switch settings.authorizationStatus {
        case .notDetermined:
            center.requestAuthorization(options: [.alert, .sound]) { granted, error in
                guard granted else {
                    if let error {
                        fputs("desktopctl: notification authorization failed: \(error)\n", stderr)
                    }
                    return
                }
                post()
            }
        case .authorized, .provisional, .ephemeral:
            post()
        case .denied:
            break
        @unknown default:
            break
        }
    }
}

@_cdecl("desktopctl_launcher_show_completion_notification")
public func desktopctl_launcher_show_completion_notification(
    _ title: UnsafePointer<CChar>?,
    _ body: UnsafePointer<CChar>?,
    _ sessionID: UnsafePointer<CChar>?
) {
    guard let titlePtr = title, let bodyPtr = body, let sessionIDPtr = sessionID else { return }
    let title = String(cString: titlePtr)
    let body = String(cString: bodyPtr)
    let sessionID = String(cString: sessionIDPtr)
    postCompletionNotification(title: title, body: body, sessionID: sessionID)
}

@_cdecl("desktopctl_launcher_set_notification_action_callback")
public func desktopctl_launcher_set_notification_action_callback(
    _ callback: NotificationActionCallback?
) {
    notificationDelegate.callback = callback
    UNUserNotificationCenter.current().delegate = notificationDelegate
}

public typealias LauncherSettingsChangedCallback =
    @convention(c) (Int32, Int32, Int32, Int32) -> Void

@_cdecl("desktopctl_launcher_start_settings_observer")
public func desktopctl_launcher_start_settings_observer(
    _ callback: LauncherSettingsChangedCallback?
) {
    guard Thread.isMainThread else { return }
    let center = DistributedNotificationCenter.default()
    if let launcherSettingsObserver {
        center.removeObserver(launcherSettingsObserver)
    }
    launcherSettingsObserver = center.addObserver(
        forName: Notification.Name("com.desktopctl.launcher.settings.changed"),
        object: nil,
        queue: .main
    ) { notification in
        guard let userInfo = notification.userInfo,
              let keyCode = (userInfo["key_code"] as? NSNumber)?.int32Value,
              let modifiers = (userInfo["modifiers"] as? NSNumber)?.int32Value,
              let renderKeyboardShortcuts =
                  (userInfo["render_keyboard_shortcuts"] as? NSNumber)?.int32Value,
              let useNativeNotifications =
                  (userInfo["use_native_notifications"] as? NSNumber)?.int32Value
        else { return }
        callback?(keyCode, modifiers, renderKeyboardShortcuts, useNativeNotifications)
    }
}

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
    actionMenuEventMonitor = NSEvent.addLocalMonitorForEvents(
        matching: [.leftMouseDown, .rightMouseDown]
    ) { event in
        if let actionMenuPanel, actionMenuPanel.isVisible, event.window !== actionMenuPanel {
            _ = nextModel.dismissActionsMenu()
        }
        return event
    }
    actionMenuKeyEventMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
        if let actionMenuPanel,
           actionMenuPanel.isVisible,
           nextModel.showActionsMenu
        {
            return handleActionMenuKeyEvent(event, model: nextModel) ? nil : event
        }
        return handleLauncherTabEvent(event, model: nextModel) ? nil : event
    }
    actionMenuActivationObserver = NotificationCenter.default.addObserver(
        forName: NSWindow.didBecomeKeyNotification,
        object: nil,
        queue: .main
    ) { notification in
        guard let parent = actionMenuParent,
              notification.object as AnyObject? === parent
        else { return }
        DispatchQueue.main.async {
            refocusVisibleActionMenu()
        }
    }
    return true
}

@_cdecl("desktopctl_launcher_set_snapshot")
public func desktopctl_launcher_set_snapshot(
    _ json: UnsafePointer<CChar>?,
    _ length: Int
) {
    guard let json, length >= 0 else { return }
    let data = Data(bytes: json, count: length)
    DispatchQueue.main.async {
        model?.applySnapshot(data)
    }
}

@_cdecl("desktopctl_launcher_unmount")
public func desktopctl_launcher_unmount() {
    guard Thread.isMainThread else { return }
    if let monitor = scrollWheelMonitor {
        NSEvent.removeMonitor(monitor)
        scrollWheelMonitor = nil
    }
    if let monitor = actionMenuEventMonitor {
        NSEvent.removeMonitor(monitor)
        actionMenuEventMonitor = nil
    }
    if let monitor = actionMenuKeyEventMonitor {
        NSEvent.removeMonitor(monitor)
        actionMenuKeyEventMonitor = nil
    }
    if let observer = actionMenuActivationObserver {
        NotificationCenter.default.removeObserver(observer)
        actionMenuActivationObserver = nil
    }
    hideActionMenuPanel()
    actionMenuPanel = nil
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
    model?.toggleActionsMenuFromKeyboard()
}

@_cdecl("desktopctl_launcher_move_actions_menu_focus")
public func desktopctl_launcher_move_actions_menu_focus(
    _ delta: Int,
    _ escapeTop: Bool
) -> Bool {
    guard Thread.isMainThread else { return false }
    return model?.moveActionsMenuFocus(delta: delta, escapeToPromptAtTop: escapeTop) ?? false
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
