import AppKit
import Carbon.HIToolbox
import UserNotifications
import WebKit

// Agent Box: a thin native shell. Left pane is the box's web dashboard,
// right pane is the selected chat's claude.ai page, bottom is that chat's
// terminal. Everything heavy runs on the box; this window reaches it through
// an ssh tunnel.

let sshHost = UserDefaults.standard.string(forKey: "sshHost") ?? "coder.tj-agents"
let dashPort = 18790
let ttyPort = 18791
let agentBoxDir = (NSHomeDirectory() as NSString).appendingPathComponent("dev/agent-box")

final class Tunnel {
    private var process: Process?
    private var stopping = false

    func start() {
        stopping = false
        launch()
    }

    func stop() {
        stopping = true
        process?.terminate()
    }

    private func launch() {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: "/usr/bin/ssh")
        p.arguments = [
            "-N", "-o", "ExitOnForwardFailure=yes", "-o", "ServerAliveInterval=15",
            "-o", "ServerAliveCountMax=3",
            "-L", "\(dashPort):127.0.0.1:8790", "-L", "\(ttyPort):127.0.0.1:8791", sshHost,
        ]
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin"
        p.environment = env
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        p.terminationHandler = { [weak self] _ in
            guard let self, !self.stopping else { return }
            DispatchQueue.main.asyncAfter(deadline: .now() + 3) { self.launch() }
        }
        try? p.run()
        process = p
    }
}

final class Shell: NSObject, NSApplicationDelegate, WKScriptMessageHandler, WKUIDelegate, WKNavigationDelegate {
    var window: NSWindow!
    var dash: WKWebView!
    var chat: WKWebView!
    var term: WKWebView!
    var rightSplit: NSSplitView!
    var statusItem: NSStatusItem!
    let tunnel = Tunnel()
    var lastNeeds = 0
    var selectedTerm: String?
    var hotKey: EventHotKeyRef?

    func applicationDidFinishLaunching(_ notification: Notification) {
        tunnel.start()

        let dashConfig = WKWebViewConfiguration()
        dashConfig.userContentController.add(self, name: "agentbox")
        dash = makeWeb(dashConfig)
        chat = makeWeb(WKWebViewConfiguration())
        term = makeWeb(WKWebViewConfiguration())

        rightSplit = NSSplitView()
        rightSplit.isVertical = false
        rightSplit.dividerStyle = .thin
        rightSplit.addArrangedSubview(chat)
        rightSplit.addArrangedSubview(term)

        let main = NSSplitView()
        main.isVertical = true
        main.dividerStyle = .thin
        main.autosaveName = "AgentBoxMain"
        main.addArrangedSubview(dash)
        main.addArrangedSubview(rightSplit)
        rightSplit.autosaveName = "AgentBoxRight"

        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 1500, height: 940),
            styleMask: [.titled, .closable, .resizable, .miniaturizable],
            backing: .buffered, defer: false)
        window.title = "Agent Box"
        window.contentView = main
        window.setFrameAutosaveName("AgentBoxWindow")
        if window.frame.width < 600 { window.center() }
        window.makeKeyAndOrderFront(nil)
        DispatchQueue.main.async {
            if main.subviews[0].frame.width < 50 || main.subviews[0].frame.width > 1200 {
                main.setPosition(520, ofDividerAt: 0)
            }
            if self.rightSplit.subviews[1].frame.height < 1 {
                self.rightSplit.setPosition(self.rightSplit.frame.height * 0.72, ofDividerAt: 0)
            }
        }

        chat.load(URLRequest(url: URL(string: "https://claude.ai/code")!))
        term.loadHTMLString(placeholder("Select a chat to attach its terminal."), baseURL: nil)
        loadDashboard()

        setupMenu()
        setupStatusItem()
        setupHotKey()
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .badge, .sound]) { _, _ in }
        NSApp.activate(ignoringOtherApps: true)
        if let dir = ProcessInfo.processInfo.environment["AGENTBOX_SNAPSHOT_DIR"] {
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { self.snapshot(to: dir) }
        }
    }

    // Debugging aid: AGENTBOX_SNAPSHOT_DIR=<dir> writes each pane as a PNG.
    func snapshot(to dir: String) {
        for (name, view) in [("dash", dash!), ("chat", chat!), ("term", term!)] {
            view.takeSnapshot(with: nil) { image, _ in
                guard let image, let tiff = image.tiffRepresentation,
                      let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) else { return }
                try? png.write(to: URL(fileURLWithPath: "\(dir)/\(name).png"))
            }
        }
    }

    func applicationWillTerminate(_ notification: Notification) {
        tunnel.stop()
    }

    func applicationShouldHandleReopen(_ sender: NSApplication, hasVisibleWindows flag: Bool) -> Bool {
        window.makeKeyAndOrderFront(nil)
        return true
    }

    func makeWeb(_ config: WKWebViewConfiguration) -> WKWebView {
        config.websiteDataStore = .default()
        let w = WKWebView(frame: .zero, configuration: config)
        w.uiDelegate = self
        w.navigationDelegate = self
        w.allowsBackForwardNavigationGestures = true
        return w
    }

    func placeholder(_ text: String) -> String {
        "<html><body style='margin:0;display:flex;align-items:center;justify-content:center;height:100vh;"
            + "font:13px -apple-system;color:#888;background:#161614'>\(text)</body></html>"
    }

    func loadDashboard() {
        dash.load(URLRequest(url: URL(string: "http://127.0.0.1:\(dashPort)/")!))
    }

    // The tunnel takes a moment to come up; retry the dashboard until it does.
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        if webView === dash {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { self.loadDashboard() }
        } else if webView === term, let s = selectedTerm {
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { self.attachTerminal(s) }
        }
    }

    func attachTerminal(_ session: String) {
        selectedTerm = session
        term.load(URLRequest(url: URL(string: "http://127.0.0.1:\(ttyPort)/?arg=\(session)")!))
    }

    // Links meant for a new window, and app URLs (cursor://, vscode://), go to macOS.
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for navigationAction: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        if let url = navigationAction.request.url {
            if webView === chat, url.host?.hasSuffix("claude.ai") == true || url.host?.contains("google") == true
                || url.host?.contains("apple") == true {
                webView.load(navigationAction.request)
            } else {
                NSWorkspace.shared.open(url)
            }
        }
        return nil
    }

    func webView(_ webView: WKWebView, decidePolicyFor navigationAction: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard let url = navigationAction.request.url, let scheme = url.scheme else {
            return decisionHandler(.allow)
        }
        if !["http", "https", "about", "data", "blob"].contains(scheme) {
            NSWorkspace.shared.open(url)
            return decisionHandler(.cancel)
        }
        if webView === dash, navigationAction.navigationType == .linkActivated,
           url.host != "127.0.0.1" {
            NSWorkspace.shared.open(url)
            return decisionHandler(.cancel)
        }
        decisionHandler(.allow)
    }

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let body = message.body as? [String: Any], let type = body["type"] as? String else { return }
        switch type {
        case "select":
            if let c = body["chat"] as? [String: Any] {
                if let u = c["rc_url"] as? String, let url = URL(string: u) {
                    chat.load(URLRequest(url: url))
                }
                if let id = c["id"] as? String, (c["host"] as? String) == "box" {
                    attachTerminal("chat-" + String(id.prefix(8)))
                } else {
                    selectedTerm = nil
                    term.loadHTMLString(placeholder("This chat is on your Mac."), baseURL: nil)
                }
            } else if let mr = body["mr"] as? String, let url = URL(string: mr) {
                NSWorkspace.shared.open(url)
            }
        case "status":
            let needs = body["needs"] as? Int ?? 0
            updateStatus(needs)
        case "run":
            if let args = body["args"] as? [String], let id = body["reqId"] as? String {
                runLocal(args, reqId: id)
            }
        default:
            break
        }
    }

    // Runs one of agent-box's own scripts on the Mac (swap, test locally).
    func runLocal(_ args: [String], reqId: String) {
        guard let tool = args.first, ["chat", "wip-restore", "env-push"].contains(tool) else { return }
        let p = Process()
        p.executableURL = URL(fileURLWithPath: (agentBoxDir as NSString).appendingPathComponent("bin/" + tool))
        p.arguments = Array(args.dropFirst())
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = "\(NSHomeDirectory())/.local/bin:\(NSHomeDirectory())/dev/redo-monorepo-worktrees/bin:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin"
        p.environment = env
        let out = Pipe()
        p.standardOutput = out
        p.standardError = out
        p.terminationHandler = { proc in
            let text = String(data: out.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
            let payload = (try? JSONSerialization.data(withJSONObject: ["reqId": reqId, "code": proc.terminationStatus, "output": text]))
                .flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
            DispatchQueue.main.async {
                self.dash.evaluateJavaScript("window.agentboxResult && window.agentboxResult(\(payload))")
            }
        }
        do { try p.run() } catch {
            dash.evaluateJavaScript("window.agentboxResult && window.agentboxResult({reqId: '\(reqId)', code: 127, output: 'could not start \(tool)'})")
        }
    }

    func updateStatus(_ needs: Int) {
        statusItem.button?.title = needs > 0 ? "✋\(needs)" : "◇"
        NSApp.dockTile.badgeLabel = needs > 0 ? "\(needs)" : nil
        if needs > lastNeeds {
            let content = UNMutableNotificationContent()
            content.title = "Agent Box"
            content.body = needs == 1 ? "1 thing needs you" : "\(needs) things need you"
            UNUserNotificationCenter.current().add(
                UNNotificationRequest(identifier: UUID().uuidString, content: content, trigger: nil))
        }
        lastNeeds = needs
    }

    func setupStatusItem() {
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        statusItem.button?.title = "◇"
        statusItem.button?.target = self
        statusItem.button?.action = #selector(showWindow)
    }

    @objc func showWindow() {
        NSApp.activate(ignoringOtherApps: true)
        window.makeKeyAndOrderFront(nil)
    }

    @objc func toggleTerminal() {
        let t = rightSplit.subviews[1]
        rightSplit.setPosition(t.frame.height < 2 ? rightSplit.frame.height * 0.72 : rightSplit.frame.height, ofDividerAt: 0)
    }

    @objc func reloadAll() {
        loadDashboard()
        chat.reload()
        if let s = selectedTerm { attachTerminal(s) }
    }

    @objc func focusDash() { window.makeFirstResponder(dash) }
    @objc func focusChat() { window.makeFirstResponder(chat) }
    @objc func focusTerm() { window.makeFirstResponder(term) }

    func setupMenu() {
        let menu = NSMenu()
        let appItem = NSMenuItem()
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Quit Agent Box", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        menu.addItem(appItem)

        let editItem = NSMenuItem()
        let edit = NSMenu(title: "Edit")
        edit.addItem(withTitle: "Undo", action: Selector(("undo:")), keyEquivalent: "z")
        edit.addItem(withTitle: "Redo", action: Selector(("redo:")), keyEquivalent: "Z")
        edit.addItem(.separator())
        edit.addItem(withTitle: "Cut", action: #selector(NSText.cut(_:)), keyEquivalent: "x")
        edit.addItem(withTitle: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")
        edit.addItem(withTitle: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
        edit.addItem(withTitle: "Select All", action: #selector(NSText.selectAll(_:)), keyEquivalent: "a")
        editItem.submenu = edit
        menu.addItem(editItem)

        let viewItem = NSMenuItem()
        let view = NSMenu(title: "View")
        view.addItem(withTitle: "Dashboard", action: #selector(focusDash), keyEquivalent: "1")
        view.addItem(withTitle: "Chat", action: #selector(focusChat), keyEquivalent: "2")
        view.addItem(withTitle: "Terminal", action: #selector(focusTerm), keyEquivalent: "3")
        view.addItem(withTitle: "Toggle Terminal", action: #selector(toggleTerminal), keyEquivalent: "j")
        view.addItem(withTitle: "Reload", action: #selector(reloadAll), keyEquivalent: "r")
        view.items.forEach { $0.target = self }
        viewItem.submenu = view
        menu.addItem(viewItem)
        NSApp.mainMenu = menu
    }

    // ⌃⌥Space brings the window forward from anywhere.
    func setupHotKey() {
        var spec = EventTypeSpec(eventClass: OSType(kEventClassKeyboard), eventKind: UInt32(kEventHotKeyPressed))
        InstallEventHandler(GetApplicationEventTarget(), { _, _, ctx in
            let me = Unmanaged<Shell>.fromOpaque(ctx!).takeUnretainedValue()
            DispatchQueue.main.async { me.showWindow() }
            return noErr
        }, 1, &spec, Unmanaged.passUnretained(self).toOpaque(), nil)
        let id = EventHotKeyID(signature: OSType(0x4142_4F58), id: 1)
        RegisterEventHotKey(UInt32(kVK_Space), UInt32(controlKey | optionKey), id, GetApplicationEventTarget(), 0, &hotKey)
    }
}

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let shell = Shell()
app.delegate = shell
app.run()
