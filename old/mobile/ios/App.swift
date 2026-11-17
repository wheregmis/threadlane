import UIKit

@main
final class AppDelegate: UIResponder, UIApplicationDelegate {
    func application(_ application: UIApplication,
                     configurationForConnecting connectingSceneSession: UISceneSession,
                     options: UIScene.ConnectionOptions) -> UISceneConfiguration {
        let config = UISceneConfiguration(name: "Default Configuration",
                                          sessionRole: connectingSceneSession.role)
        config.delegateClass = SceneDelegate.self
        return config
    }
}

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?

    func scene(_ scene: UIScene,
               willConnectTo session: UISceneSession,
               options connectionOptions: UIScene.ConnectionOptions) {
        guard let windowScene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: windowScene)
        window.rootViewController = ThreadlaneContainerController()
        window.makeKeyAndVisible()
        self.window = window
        // Cold-launch deep link (QR pairing via `threadlane://pair?...`).
        if let url = connectionOptions.urlContexts.first?.url {
            Self.handleOpenURL(url)
        }
    }

    func sceneDidBecomeActive(_ scene: UIScene) {
        gpui_ios_did_become_active(nil)
        threadlane_mobile_did_become_active()
    }

    func sceneWillResignActive(_ scene: UIScene) {
        gpui_ios_will_resign_active(nil)
    }

    /// QR pairing: the Camera app opens `threadlane://pair?...` here and
    /// Rust applies it via the deep-link handler.
    func scene(_ scene: UIScene, openURLContexts urlContexts: Set<UIOpenURLContext>) {
        guard let url = urlContexts.first?.url else { return }
        Self.handleOpenURL(url)
    }

    private static func handleOpenURL(_ url: URL) {
        gpui_ios_handle_open_url(Unmanaged.passUnretained(url.absoluteString as NSString).toOpaque())
    }
}

/// UIKit owns this view's frame; GPUI owns only the content rendered inside it.
final class GPUITextView: UIView {
    private let gpuiWindow: UnsafeMutableRawPointer
    let contentController: UIViewController

    override init(frame: CGRect) {
        gpui_ios_set_embedded()
        gpui_ios_register_app()
        gpui_ios_run_demo()
        guard let window = gpui_ios_get_window(),
              let controller = gpui_ios_view_controller(window) else {
            fatalError("Could not create the GPUI view")
        }
        gpuiWindow = window
        contentController = Unmanaged<UIViewController>.fromOpaque(controller).takeUnretainedValue()
        super.init(frame: frame)
        clipsToBounds = true
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is unsupported") }

    func attach(to parent: UIViewController) {
        parent.addChild(contentController)
        addSubview(contentController.view)
        contentController.didMove(toParent: parent)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        guard bounds.width > 0, bounds.height > 0,
              contentController.view.frame != bounds else { return }
        // Publish the new geometry and its rendered content together. Otherwise
        // Core Animation can stretch the previous drawable until the next tick.
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        contentController.view.frame = bounds
        gpui_ios_layout_view(gpuiWindow)
        drawFrame()
        CATransaction.commit()
    }

    func drawFrame() { gpui_ios_request_frame(gpuiWindow) }
}

final class ThreadlaneContainerController: UIViewController {
    private var gpuiView: GPUITextView!
    private var displayLink: CADisplayLink?

    override func viewDidLoad() {
        super.viewDidLoad()
        overrideUserInterfaceStyle = .dark
        view.backgroundColor = .systemBackground

        gpuiView = GPUITextView(frame: .zero)
        gpuiView.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(gpuiView)
        gpuiView.attach(to: self)

        NSLayoutConstraint.activate([
            gpuiView.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
            gpuiView.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            gpuiView.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            gpuiView.bottomAnchor.constraint(equalTo: view.keyboardLayoutGuide.topAnchor),
        ])
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        displayLink = CADisplayLink(target: self, selector: #selector(renderFrame))
        displayLink?.add(to: .main, forMode: .common)
    }

    override func viewWillDisappear(_ animated: Bool) {
        displayLink?.invalidate()
        displayLink = nil
        super.viewWillDisappear(animated)
    }

    @objc private func renderFrame() { gpuiView.drawFrame() }
}
