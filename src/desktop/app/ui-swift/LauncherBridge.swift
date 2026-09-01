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
    @Published private(set) var isScrolling = false
    private var scrollGeneration = 0
    private var preserveScrollForNextSelection = false
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
            emit(["type": "follow_up", "session_id": renderState.sessionID, "prompt": value])
        } else {
            emit(["type": "new_request", "prompt": value])
        }
        prompt = ""
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

    func toggleActionsMenu() {
        withAnimation(.easeOut(duration: 0.16)) {
            showActionsMenu.toggle()
        }
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
        showAllFocused = false
        preserveScrollForNextSelection = false
        showActionsMenu = false
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
            guard commandSelector == #selector(NSResponder.insertNewline(_:)),
                  let event = NSApp.currentEvent
            else {
                return false
            }

            let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
            let nonCommandModifiers = modifiers.subtracting([.command, .numericPad])
            guard modifiers.contains(.command), nonCommandModifiers.isEmpty else {
                return false
            }

            parent?.onCommandReturn?()
            return true
        }

        func controlTextDidBeginEditing(_ obj: Notification) {
            guard let parent else { return }
            if !parent.isFocused.wrappedValue {
                parent.isFocused.wrappedValue = true
            }
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
        .overlay {
            if model.showActionsMenu {
                Color.clear
                    .contentShape(Rectangle())
                    .onTapGesture { _ = model.dismissActionsMenu() }
            }
        }
        .overlay(alignment: .topTrailing) {
            if model.renderState.screen != "Session", model.showActionsMenu {
                actionsMenu
                    .padding(.trailing, LauncherTheme.Spacing.lg)
                    .padding(.top, 46)
                    .transition(
                        .opacity.combined(
                            with: .scale(scale: 0.94, anchor: .topTrailing)
                        )
                    )
            }
        }
        .overlay(alignment: .bottomTrailing) {
            if model.renderState.screen == "Session", model.showActionsMenu {
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
        LauncherPillButton(action: model.toggleActionsMenu) {
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
                    if model.renderState.renderKeyboardShortcuts {
                        HStack(spacing: 2) {
                            LauncherKeyCap(title: "⌘")
                            LauncherKeyCap(title: ",")
                        }
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
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
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
                            HStack {
                                if message.user { Spacer(minLength: 42) }
                                Text(message.text)
                                    .textSelection(.enabled)
                                    .padding(.horizontal, 13)
                                    .padding(.vertical, 9)
                                    .foregroundColor(message.user ? .white : .primary)
                                    .background {
                                        ZStack(alignment: message.user ? .bottomTrailing : .bottomLeading) {
                                            RoundedRectangle(cornerRadius: 17, style: .continuous)
                                                .fill(message.user ? Color(nsColor: .systemBlue) : Color.primary)
                                            SessionBubbleTail(pointsRight: message.user)
                                                .fill(message.user ? Color(nsColor: .systemBlue) : Color.primary)
                                                .frame(width: 25, height: 14)
                                                .offset(x: message.user ? 3 : -3, y: 0)
                                        }
                                        .compositingGroup()
                                        .opacity(message.user ? 1.0 : 0.10)
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
                    }
                    .padding(.top, 4)
                    .padding(.bottom, 8)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .onAppear {
                    guard let last = model.renderState.messages.indices.last else { return }
                    DispatchQueue.main.async {
                        proxy.scrollTo(last, anchor: .bottom)
                    }
                }
                .onChange(of: model.renderState.messages.count) { _ in
                    guard let last = model.renderState.messages.indices.last else { return }
                    DispatchQueue.main.async {
                        proxy.scrollTo(last, anchor: .bottom)
                    }
                }
            }

            Rectangle()
                .fill(LauncherTheme.textTertiary.opacity(0.24))
                .frame(height: 0.5)
                .frame(maxWidth: .infinity)
                .padding(.horizontal, -LauncherTheme.Spacing.lg)

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
                    AppKitPromptField(
                        placeholder: "Follow up…",
                        text: $model.prompt,
                        isFocused: $promptFocused,
                        onSubmit: { model.sendPrompt() },
                        onCommandReturn: model.openInGhostty
                    )
                    .frame(maxWidth: .infinity)
                    .frame(height: LauncherTheme.controlHeight)
                    sessionActionsButton
                }
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
