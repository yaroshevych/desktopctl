import Foundation

// Communicates with the running desktopctld daemon via its Unix socket.
// Used to query permission state from the daemon process, which holds the correct OS permissions,
// rather than querying permission APIs (like CGPreflightScreenCaptureAccess) from this dialog
// process — calling those from a process without screen recording permission triggers macOS 15
// system prompts.
enum DaemonIPC {
    struct PermissionsResult {
        let accessibility: Bool
        let screenRecording: Bool
    }

    // Sends a permissions_check command to the daemon and returns the result, or nil on failure.
    static func checkPermissions() -> PermissionsResult? {
        let paths = socketPaths()
        for path in paths {
            if let result = tryCheckPermissions(socketPath: path) {
                return result
            }
        }
        return nil
    }

    // MARK: - Private

    private static func socketPaths() -> [String] {
        if let path = ProcessInfo.processInfo.environment["DESKTOPCTL_SOCKET_PATH"] {
            return [path]
        }
        let tmp = FileManager.default.temporaryDirectory.path
        let primary = (tmp as NSString).appendingPathComponent("desktopctl/desktopctl.sock")
        return [primary, "/tmp/desktopctl.sock"]
    }

    private static func tryCheckPermissions(socketPath: String) -> PermissionsResult? {
        let sock = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
        guard sock >= 0 else { return nil }
        defer { Darwin.close(sock) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let pathBytes = socketPath.utf8
        guard pathBytes.count < MemoryLayout.size(ofValue: addr.sun_path) else { return nil }
        withUnsafeMutableBytes(of: &addr.sun_path) { buf in
            for (i, b) in pathBytes.enumerated() { buf[i] = b }
        }

        let connected = withUnsafePointer(to: addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                Darwin.connect(sock, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else { return nil }

        let request: [String: Any] = [
            "protocol_version": 1,
            "request_id": "dialog-perm-check",
            "options": ["background_input": false],
            "command": ["cmd": "permissions_check"]
        ]
        guard let payload = try? JSONSerialization.data(withJSONObject: request) else { return nil }

        // Write 4-byte big-endian length header + payload
        var lenBE = UInt32(payload.count).bigEndian
        let header = Data(bytes: &lenBE, count: 4)
        guard sendAll(sock, header) && sendAll(sock, payload) else { return nil }

        // Read 4-byte big-endian length header
        var headerBuf = [UInt8](repeating: 0, count: 4)
        guard recvAll(sock, &headerBuf) else { return nil }
        let bodyLen = Int(UInt32(bigEndian: headerBuf.withUnsafeBytes { $0.load(as: UInt32.self) }))
        guard bodyLen > 0 && bodyLen < 4 * 1024 * 1024 else { return nil }

        var bodyBuf = [UInt8](repeating: 0, count: bodyLen)
        guard recvAll(sock, &bodyBuf) else { return nil }

        guard let json = try? JSONSerialization.jsonObject(with: Data(bodyBuf)) as? [String: Any],
              let ok = json["ok"] as? Bool, ok,
              let result = json["result"] as? [String: Any] else { return nil }

        let ax = (result["accessibility"] as? [String: Any])?["granted"] as? Bool ?? false
        let sr = (result["screen_recording"] as? [String: Any])?["granted"] as? Bool ?? false
        return PermissionsResult(accessibility: ax, screenRecording: sr)
    }

    private static func sendAll(_ sock: Int32, _ data: Data) -> Bool {
        var sent = 0
        while sent < data.count {
            let n = data.withUnsafeBytes { ptr in
                Darwin.send(sock, ptr.baseAddress!.advanced(by: sent), data.count - sent, 0)
            }
            guard n > 0 else { return false }
            sent += n
        }
        return true
    }

    private static func recvAll(_ sock: Int32, _ buf: inout [UInt8]) -> Bool {
        var received = 0
        while received < buf.count {
            let n = Darwin.recv(sock, &buf[received], buf.count - received, 0)
            guard n > 0 else { return false }
            received += n
        }
        return true
    }
}
