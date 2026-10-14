import AppKit
import Carbon.HIToolbox
import UserNotifications
import WebKit

// Agent Box: a thin native shell laid out like super.engineering. The sidebar,
// tab strip, and branch bar are pages served by the box's dashboard; the chat
// is claude.ai; the terminal is the box's ttyd. Everything heavy runs on the
// box; this window reaches it through an ssh tunnel.

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
    var header: WKWebView!
    var chat: WKWebView!
    var bar: WKWebView!
    var term: WKWebView!
    var rightSplit: NSSplitView!
    var statusItem: NSStatusItem!
    let tunnel = Tunnel()
    var lastNeeds = 0
    var selectedTerm: String?
    var current: [String: Any]?
    var hotKey: EventHotKeyRef?

    // Same layout as super.engineering: sidebar | (tabs + chat) over (branch bar + terminal).
    func applicationDidFinishLaunching(_ notification: Notification) {
        tunnel.start()

        dash = makeWeb(bridged: true)
        header = makeWeb(bridged: true)
        bar = makeWeb(bridged: true)
        chat = makeWeb(bridged: false)
        term = makeWeb(bridged: false)

        let top = stack(fixed: header, height: 40, flexible: chat)
        let bottom = stack(fixed: bar, height: 72, flexible: term)

        rightSplit = NSSplitView()
        rightSplit.isVertical = false
        rightSplit.dividerStyle = .thin
        rightSplit.autosaveName = "AgentBoxRight2"
        rightSplit.addArrangedSubview(top)
        rightSplit.addArrangedSubview(bottom)

        let main = NSSplitView()
        main.isVertical = true
        main.dividerStyle = .thin
        main.autosaveName = "AgentBoxMain2"
        main.addArrangedSubview(dash)
        main.addArrangedSubview(rightSplit)

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
            let w = main.subviews[0].frame.width
            if w < 120 || w > 600 { main.setPosition(320, ofDividerAt: 0) }
            let h = self.rightSplit.subviews[1].frame.height
            if h < 80 || h > self.rightSplit.frame.height - 120 {
                self.rightSplit.setPosition(self.rightSplit.frame.height * 0.68, ofDividerAt: 0)
            }
        }

        chat.load(URLRequest(url: URL(string: "https://claude.ai/code")!))
        term.loadHTMLString(placeholder("Select a worktree to open its terminal."), baseURL: nil)
        loadLocal()

        // Keep the box's copy of the Mac's sidebar, dispatcher state, skills, and
        // memory fresh while the app is open.
        Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in self?.pushEnv() }
        pushEnv()

        setupMenu()
        setupStatusItem()
        setupHotKey()
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .badge, .sound]) { _, _ in }
        NSApp.activate(ignoringOtherApps: true)
        if let dir = ProcessInfo.processInfo.environment["AGENTBOX_SNAPSHOT_DIR"] {
            DispatchQueue.main.asyncAfter(deadline: .now() + 15) { self.snapshot(to: dir) }
        }
    }

    func stack(fixed: NSView, height: CGFloat, flexible: NSView) -> NSView {
        let v = NSView()
        for sub in [fixed, flexible] {
            sub.translatesAutoresizingMaskIntoConstraints = false
            v.addSubview(sub)
        }
        NSLayoutConstraint.activate([
            fixed.topAnchor.constraint(equalTo: v.topAnchor),
            fixed.leadingAnchor.constraint(equalTo: v.leadingAnchor),
            fixed.trailingAnchor.constraint(equalTo: v.trailingAnchor),
            fixed.heightAnchor.constraint(equalToConstant: height),
            flexible.topAnchor.constraint(equalTo: fixed.bottomAnchor),
            flexible.leadingAnchor.constraint(equalTo: v.leadingAnchor),
            flexible.trailingAnchor.constraint(equalTo: v.trailingAnchor),
            flexible.bottomAnchor.constraint(equalTo: v.bottomAnchor),
        ])
        return v
    }

    func loadLocal() {
        dash.load(URLRequest(url: URL(string: "http://127.0.0.1:\(dashPort)/")!))
        header.load(URLRequest(url: URL(string: "http://127.0.0.1:\(dashPort)/chrome?part=header")!))
        bar.load(URLRequest(url: URL(string: "http://127.0.0.1:\(dashPort)/chrome?part=bar")!))
    }

    // Debugging aid: AGENTBOX_SNAPSHOT_DIR=<dir> writes each pane as a PNG.
    func snapshot(to dir: String) {
        for (name, view) in [("dash", dash!), ("header", header!), ("chat", chat!), ("bar", bar!), ("term", term!)] {
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

    func makeWeb(bridged: Bool) -> WKWebView {
        let config = WKWebViewConfiguration()
        if bridged { config.userContentController.add(self, name: "agentbox") }
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

    // The tunnel takes a moment to come up; retry the local pages until it does.
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        if webView === dash || webView === header || webView === bar {
            let url = webView.url ?? URL(string: "http://127.0.0.1:\(dashPort)/")!
            DispatchQueue.main.asyncAfter(deadline: .now() + 2) { webView.load(URLRequest(url: url)) }
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
        if webView !== chat, webView !== term, navigationAction.navigationType == .linkActivated,
           url.host != "127.0.0.1" {
            NSWorkspace.shared.open(url)
            return decisionHandler(.cancel)
        }
        decisionHandler(.allow)
    }

    func js(_ obj: Any) -> String {
        (try? JSONSerialization.data(withJSONObject: obj)).flatMap { String(data: $0, encoding: .utf8) } ?? "null"
    }

    func show(chat c: [String: Any]?) {
        if let u = c?["rc_url"] as? String, let url = URL(string: u) {
            chat.load(URLRequest(url: url))
        } else if let sel = current {
            let mac = (sel["host"] as? String) == "mac"
            chat.loadHTMLString(placeholder(mac ? "This worktree's chat runs in super.engineering."
                : "No chat in this worktree yet. Press + above to start one."), baseURL: nil)
        }
    }

    func userContentController(_ userContentController: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let body = message.body as? [String: Any], let type = body["type"] as? String else { return }
        switch type {
        case "select":
            guard let sel = body["sel"] as? [String: Any] else { return }
            current = sel
            let first = (sel["chats"] as? [[String: Any]])?.first
            for v in [header!, bar!] { v.evaluateJavaScript("setSel(\(js(sel)), \(js(first?["id"] ?? NSNull())))") }
            show(chat: first)
            if let t = sel["term"] as? String {
                attachTerminal(t)
            } else {
                selectedTerm = nil
                term.loadHTMLString(placeholder((sel["host"] as? String) == "mac"
                    ? "Mac worktrees open in your IDE or Terminal." : "No terminal."), baseURL: nil)
            }
        case "tab":
            let c = body["chat"] as? [String: Any]
            show(chat: c)
            if let sel = current { bar.evaluateJavaScript("setSel(\(js(sel)), \(js(c?["id"] ?? NSNull())))") }
        case "newchat":
            dash.evaluateJavaScript("openNewChat(\(js(body["dir"] ?? "")))")
            window.makeFirstResponder(dash)
        case "open":
            if let u = body["url"] as? String, let url = URL(string: u) { NSWorkspace.shared.open(url) }
        case "status":
            updateStatus(body["needs"] as? Int ?? 0)
        case "run":
            if let args = body["args"] as? [String], let id = body["reqId"] as? String {
                runLocal(args, reqId: id, reply: message.webView ?? bar)
            }
        default:
            break
        }
    }

    // Runs one of agent-box's own scripts on the Mac (swap, test locally).
    func runLocal(_ args: [String], reqId: String, reply: WKWebView) {
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
                reply.evaluateJavaScript("window.agentboxResult && window.agentboxResult(\(payload))")
                self.dash.evaluateJavaScript("window.refreshSidebar && window.refreshSidebar()")
            }
        }
        do { try p.run() } catch {
            reply.evaluateJavaScript("window.agentboxResult && window.agentboxResult({reqId: '\(reqId)', code: 127, output: 'could not start \(tool)'})")
        }
    }

    func pushEnv() {
        let p = Process()
        p.executableURL = URL(fileURLWithPath: (agentBoxDir as NSString).appendingPathComponent("bin/env-push"))
        var env = ProcessInfo.processInfo.environment
        env["PATH"] = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin"
        p.environment = env
        p.standardOutput = FileHandle.nullDevice
        p.standardError = FileHandle.nullDevice
        try? p.run()
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
        loadLocal()
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
        view.addItem(withTitle: "Sidebar", action: #selector(focusDash), keyEquivalent: "1")
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
