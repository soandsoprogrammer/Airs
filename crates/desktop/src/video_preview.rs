#[cfg(target_os = "macos")]
pub use macos::VideoPreview;

#[cfg(target_os = "macos")]
mod macos {
    use anyhow::{Context, Result, ensure};
    use objc2::rc::{Retained, autoreleasepool};
    use objc2::runtime::{Bool, ProtocolObject};
    use objc2::{
        ClassType, DefinedClass, MainThreadOnly, class, define_class, extern_class, msg_send,
    };
    use objc2_app_kit::{
        NSBackingStoreType, NSResponder, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
        NSWindowTabbingMode,
    };
    use objc2_foundation::{
        MainThreadMarker, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize,
        NSString, NSURL,
    };
    use std::{ffi::CString, os::unix::ffi::OsStrExt, path::Path, ptr::NonNull};

    #[link(name = "AVFoundation", kind = "framework")]
    unsafe extern "C" {}

    #[link(name = "AVKit", kind = "framework")]
    unsafe extern "C" {}

    extern_class!(
        // SAFETY: AVPlayer is declared as an NSObject subclass in AVPlayer.h.
        #[unsafe(super(NSObject))]
        #[name = "AVPlayer"]
        struct Player;
    );

    extern_class!(
        // SAFETY: AVPlayerView is declared as an NSView subclass in AVPlayerView.h.
        #[unsafe(super(NSView, NSResponder, NSObject))]
        #[name = "AVPlayerView"]
        struct PlayerView;
    );

    impl PlayerView {
        fn new(frame: NSRect, mtm: MainThreadMarker) -> Retained<Self> {
            // AVPlayerViewControlsStyleInline places the native bar at the bottom of the view.
            const INLINE_CONTROLS: isize = 1;
            // SAFETY: AVPlayerView inherits NSView's frame initializer. AppKit calls are on the
            // main thread, and the AVKit setters take NSInteger and BOOL values respectively.
            unsafe {
                let view: Retained<Self> = msg_send![Self::alloc(mtm), initWithFrame: frame];
                let _: () = msg_send![&*view, setControlsStyle: INLINE_CONTROLS];
                let _: () = msg_send![&*view, setAllowsPictureInPicturePlayback: Bool::NO];
                let _: () = msg_send![&*view, setUpdatesNowPlayingInfoCenter: Bool::NO];
                view
            }
        }

        fn set_player(&self, player: Option<&Player>) {
            // SAFETY: AVPlayerView's `player` property retains an optional AVPlayer.
            unsafe { msg_send![self, setPlayer: player] }
        }

        fn stop(&self) {
            // SAFETY: `player` returns an optional AVPlayer and `pause` takes no arguments.
            let player: Option<Retained<Player>> = unsafe { msg_send![self, player] };
            if let Some(player) = player {
                let _: () = unsafe { msg_send![&*player, pause] };
                self.set_player(None);
            }
        }
    }

    define_class!(
        // SAFETY: NSObject has no additional subclassing requirements. The delegate has no Drop.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        #[name = "SocialPublisherVideoPreviewDelegate"]
        #[ivars = Retained<PlayerView>]
        struct PreviewDelegate;

        // SAFETY: NSObject supplies the NSObjectProtocol implementation.
        unsafe impl NSObjectProtocol for PreviewDelegate {}

        // SAFETY: The delegate implements the optional window close callback with its SDK signature.
        unsafe impl NSWindowDelegate for PreviewDelegate {
            #[unsafe(method(windowWillClose:))]
            fn window_will_close(&self, _: &NSNotification) {
                self.ivars().stop();
            }
        }
    );

    struct PreviewWindow {
        window: Retained<NSWindow>,
        delegate: Retained<PreviewDelegate>,
    }

    impl PreviewWindow {
        fn new(mtm: MainThreadMarker) -> Self {
            let size = NSSize::new(900., 520.);
            let frame = NSRect::new(NSPoint::new(0., 0.), size);
            // SAFETY: We create and retain this window on the main thread. Closing it does not
            // release our reference because setReleasedWhenClosed is disabled below.
            let window = unsafe {
                NSWindow::initWithContentRect_styleMask_backing_defer(
                    NSWindow::alloc(mtm),
                    frame,
                    NSWindowStyleMask::Titled
                        | NSWindowStyleMask::Closable
                        | NSWindowStyleMask::Resizable,
                    NSBackingStoreType::Buffered,
                    false,
                )
            };
            unsafe { window.setReleasedWhenClosed(false) };
            window.setMinSize(NSSize::new(390., 270.));
            window.setTabbingMode(NSWindowTabbingMode::Disallowed);
            window.center();

            let view = PlayerView::new(frame, mtm);
            window.setContentView(Some(&view));
            window.setAcceptsMouseMovedEvents(true);
            let delegate = PreviewDelegate::alloc(mtm).set_ivars(view);
            // SAFETY: The delegate has initialized ivars and NSObject's initializer takes no arguments.
            let delegate: Retained<PreviewDelegate> = unsafe { msg_send![super(delegate), init] };
            window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
            Self { window, delegate }
        }

        fn show(&self, player: &Player, path: &Path) {
            let view = self.delegate.ivars();
            view.stop();
            view.set_player(Some(player));
            let name = path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy();
            self.window
                .setTitle(&NSString::from_str(&format!("视频预览 · {name}")));
            self.window.makeKeyAndOrderFront(None);
            self.window.makeFirstResponder(Some(view));
        }
    }

    impl Drop for PreviewWindow {
        fn drop(&mut self) {
            self.delegate.ivars().stop();
            self.window.setDelegate(None);
            self.window.close();
        }
    }

    #[derive(Default)]
    pub struct VideoPreview {
        window: Option<PreviewWindow>,
    }

    impl VideoPreview {
        /// Inspect the original file on a background thread before opening the player.
        pub fn check(path: &Path) -> Result<()> {
            let metadata = path
                .metadata()
                .context("无法读取视频文件，请确认素材仍在原位置")?;
            ensure!(
                metadata.is_file() && metadata.len() > 0,
                "视频文件为空或无法读取"
            );
            autoreleasepool(|_| {
                let url = file_url(path)?;
                // SAFETY: AVURLAsset's assetWithURL creates a local URL asset. isPlayable is a
                // read-only AVAsset property and is safe to inspect off the main thread.
                let asset: Retained<NSObject> =
                    unsafe { msg_send![class!(AVURLAsset), assetWithURL: &*url] };
                let playable: Bool = unsafe { msg_send![&*asset, isPlayable] };
                ensure!(
                    playable.as_bool(),
                    "此视频无法在 macOS 中预览。可以尝试使用 H.264 编码的 MP4 视频。"
                );
                Ok(())
            })
        }

        /// Show a native player for a checked file. A new player starts paused.
        pub fn open(&mut self, path: &Path) -> Result<()> {
            let mtm = MainThreadMarker::new().context("无法在当前线程打开视频预览")?;
            let url = file_url(path)?;
            // SAFETY: playerWithURL takes an NSURL and returns a retained AVPlayer wrapper.
            let player: Retained<Player> =
                unsafe { msg_send![Player::class(), playerWithURL: &*url] };
            let window = self.window.get_or_insert_with(|| PreviewWindow::new(mtm));
            window.show(&player, path);
            Ok(())
        }
    }

    fn file_url(path: &Path) -> Result<Retained<NSURL>> {
        let path = CString::new(path.as_os_str().as_bytes()).context("视频路径无效")?;
        let ptr = NonNull::from_ref(&path.as_bytes_with_nul()[0]);
        // SAFETY: CString supplies a valid NUL-terminated filesystem representation. Foundation
        // copies the bytes before returning the URL.
        Ok(unsafe {
            NSURL::fileURLWithFileSystemRepresentation_isDirectory_relativeToURL(
                ptr.cast(),
                false,
                None,
            )
        })
    }
}

#[cfg(not(target_os = "macos"))]
#[derive(Default)]
pub struct VideoPreview {
    _unavailable: (),
}

#[cfg(not(target_os = "macos"))]
impl VideoPreview {
    pub fn check(_: &std::path::Path) -> anyhow::Result<()> {
        anyhow::bail!("应用内视频预览目前支持 macOS")
    }

    pub fn open(&mut self, path: &std::path::Path) -> anyhow::Result<()> {
        Self::check(path)
    }
}
