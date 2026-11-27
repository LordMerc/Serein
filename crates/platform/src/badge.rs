//! Native taskbar/Dock unread indicator; no message text or account identity leaves the app.
pub const fn supported() -> bool {
	cfg!(any(target_os = "windows", target_os = "macos"))
}
/// Installs, once, a watcher for the messages that make Windows forget a taskbar overlay.
/// Without one, the badge is only re-applied when the count changes.
#[cfg(not(target_os = "windows"))]
pub fn watch(_window: &winit::window::Window) {}
/// True once after the taskbar button was (re)created, so the badge must be set again.
#[cfg(not(target_os = "windows"))]
pub fn take_reapply() -> bool {
	false
}
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
pub fn set(_window: &winit::window::Window, _count: u32) -> Result<(), &'static str> {
	Err("App icon badges are currently available on Windows and macOS.")
}
#[cfg(target_os = "macos")]
pub fn set(_window: &winit::window::Window, count: u32) -> Result<(), &'static str> {
	use objc2_app_kit::NSApplication;
	use objc2_foundation::{MainThreadMarker, NSString};
	let mtm = MainThreadMarker::new().ok_or("The Dock badge requires the main thread.")?;
	let label = match count {
		0 => None,
		1..=99 => Some(NSString::from_str(&count.to_string())),
		_ => Some(NSString::from_str("99+")),
	};
	NSApplication::sharedApplication(mtm)
		.dockTile()
		.setBadgeLabel(label.as_deref());
	Ok(())
}
#[cfg(target_os = "windows")]
static REAPPLY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
#[cfg(target_os = "windows")]
static BUTTON_CREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
#[cfg(target_os = "windows")]
static TASKBAR_CREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
#[cfg(target_os = "windows")]
static WATCHING: std::sync::Once = std::sync::Once::new();
/// Installs, once, a watcher for the messages that make Windows forget a taskbar overlay:
/// `TaskbarButtonCreated` (the button now exists) and `TaskbarCreated` (Explorer restarted).
/// Without it the badge is only re-applied when the count changes.
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub fn watch(window: &winit::window::Window) {
	use std::sync::atomic::Ordering;
	use windows::{
		Win32::{
			Foundation::HWND,
			UI::{Shell::SetWindowSubclass, WindowsAndMessaging::RegisterWindowMessageW},
		},
		core::w,
	};
	use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
	let Ok(handle) = window.window_handle() else {
		return;
	};
	let RawWindowHandle::Win32(handle) = handle.as_raw() else {
		return;
	};
	WATCHING.call_once(|| {
		// SAFETY: registers two fixed system message names, then subclasses the UI-thread
		// window; the callback only touches statics and is removed on WM_NCDESTROY.
		unsafe {
			BUTTON_CREATED.store(
				RegisterWindowMessageW(w!("TaskbarButtonCreated")),
				Ordering::Relaxed,
			);
			TASKBAR_CREATED.store(
				RegisterWindowMessageW(w!("TaskbarCreated")),
				Ordering::Relaxed,
			);
			let _ = SetWindowSubclass(
				HWND(handle.hwnd.get() as *mut _),
				Some(watch_proc),
				0x5345_4252,
				0,
			);
		}
	});
}
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
unsafe extern "system" fn watch_proc(
	hwnd: windows::Win32::Foundation::HWND,
	message: u32,
	wparam: windows::Win32::Foundation::WPARAM,
	lparam: windows::Win32::Foundation::LPARAM,
	id: usize,
	_data: usize,
) -> windows::Win32::Foundation::LRESULT {
	use std::sync::atomic::Ordering;
	use windows::Win32::UI::{
		Shell::{DefSubclassProc, RemoveWindowSubclass},
		WindowsAndMessaging::WM_NCDESTROY,
	};
	if message != 0
		&& (message == BUTTON_CREATED.load(Ordering::Relaxed)
			|| message == TASKBAR_CREATED.load(Ordering::Relaxed))
	{
		REAPPLY.store(true, Ordering::Relaxed);
	}
	if message == WM_NCDESTROY {
		// SAFETY: removes this subclass from the window being destroyed.
		let _ = unsafe { RemoveWindowSubclass(hwnd, Some(watch_proc), id) };
	}
	// SAFETY: forwards the unhandled message down the subclass chain.
	unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
/// True once after the taskbar button was (re)created, so the badge must be set again.
#[cfg(target_os = "windows")]
pub fn take_reapply() -> bool {
	REAPPLY.swap(false, std::sync::atomic::Ordering::Relaxed)
}
#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
pub fn set(window: &winit::window::Window, count: u32) -> Result<(), &'static str> {
	use windows::{
		Win32::{
			Foundation::HWND,
			System::Com::{
				CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
				CoUninitialize,
			},
			UI::{
				Shell::{ITaskbarList3, TaskbarList},
				WindowsAndMessaging::{DestroyIcon, HICON},
			},
		},
		core::w,
	};
	use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
	// Each stage has its own text so a diagnostic log shows which one Windows refused.
	const HANDLE: &str = "Taskbar badge unavailable: no window handle.";
	const TASKBAR: &str = "Taskbar badge unavailable: the Windows taskbar did not respond.";
	const ICON: &str = "Taskbar badge unavailable: the badge image could not be made.";
	const OVERLAY: &str =
		"Taskbar badge unavailable: Windows refused it. Check your Windows taskbar settings.";
	let RawWindowHandle::Win32(handle) = window.window_handle().map_err(|_| HANDLE)?.as_raw()
	else {
		return Err(HANDLE);
	};
	// SAFETY: all COM operations and the retained winit handle stay on the UI thread.
	let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
	let result = (|| {
		let taskbar: ITaskbarList3 =
			unsafe { CoCreateInstance(&TaskbarList, None, CLSCTX_INPROC_SERVER) }
				.map_err(|_| TASKBAR)?;
		unsafe { taskbar.HrInit() }.map_err(|_| TASKBAR)?;
		let icon = if count > 0 {
			// The tray's DIB-section icon path: an icon from CreateIcon's 32-bit
			// device-dependent bitmap was never seen on the taskbar.
			let pixels = count_icon(count);
			// SAFETY: the buffer is exactly 16x16 premultiplied BGRA pixels, top-down.
			unsafe { crate::tray::native::create_hicon_from_bgra(16, 16, &pixels) }.ok_or(ICON)?
		} else {
			HICON::default()
		};
		// SAFETY: SetOverlayIcon copies the icon; its owned handle is destroyed immediately after.
		let result = unsafe {
			taskbar.SetOverlayIcon(HWND(handle.hwnd.get() as *mut _), icon, w!("Direct pings"))
		}
		.map_err(|_| OVERLAY);
		if count > 0 {
			let _ = unsafe { DestroyIcon(icon) };
		}
		result
	})();
	// Balance only successful apartment initialization; an existing different apartment is untouched.
	if initialized {
		unsafe { CoUninitialize() };
	}
	result
}
#[cfg(target_os = "windows")]
fn count_icon(count: u32) -> [u8; 1024] {
	const DIGITS: [[u8; 7]; 10] = [
		[14, 17, 19, 21, 25, 17, 14],
		[4, 12, 4, 4, 4, 4, 14],
		[14, 17, 1, 2, 4, 8, 31],
		[30, 1, 1, 14, 1, 1, 30],
		[2, 6, 10, 18, 31, 2, 2],
		[31, 16, 16, 30, 1, 1, 30],
		[14, 16, 16, 30, 17, 17, 14],
		[31, 1, 2, 4, 8, 8, 8],
		[14, 17, 17, 14, 17, 17, 14],
		[14, 17, 17, 15, 1, 1, 14],
	];
	let mut pixels = [0; 1024];
	let mut paint = |x: usize, y: usize, color: [u8; 4]| {
		pixels[(y * 16 + x) * 4..(y * 16 + x + 1) * 4].copy_from_slice(&color);
	};
	// Sample around the actual 16-pixel center; the previous offset clipped the right edge.
	// HICON expects premultiplied BGRA for partially transparent edge pixels.
	for y in 0..16 {
		for x in 0..16 {
			let coverage = (0..4)
				.flat_map(|sy| (0..4).map(move |sx| (sx, sy)))
				.filter(|(sx, sy)| {
					let dx = (2 * (x * 4 + sx) + 1) as i32 - 64;
					let dy = (2 * (y * 4 + sy) + 1) as i32 - 64;
					dx * dx + dy * dy <= 58 * 58
				})
				.count();
			if coverage > 0 {
				let alpha = (coverage * 255 / 16) as u8;
				paint(
					x,
					y,
					[
						(67 * u32::from(alpha) / 255) as u8,
						(63 * u32::from(alpha) / 255) as u8,
						(242 * u32::from(alpha) / 255) as u8,
						alpha,
					],
				);
			}
		}
	}
	let mut digit = |value: usize, start_x: usize| {
		for (y, row) in DIGITS[value].iter().enumerate() {
			for x in 0..5 {
				if row & (1 << (4 - x)) != 0 {
					paint(start_x + x, y + 4, [255, 255, 255, 255]);
				}
			}
		}
	};
	if count < 10 {
		digit(count as usize, 5);
	} else if count < 100 {
		digit((count / 10) as usize, 2);
		digit((count % 10) as usize, 9);
	} else {
		// Three 3x5 glyphs fit inside the same circle for 99+.
		for start_x in [2, 6] {
			for (y, row) in [7u8, 5, 7, 1, 7].iter().enumerate() {
				for x in 0..3 {
					if row & (1 << (2 - x)) != 0 {
						paint(start_x + x, y + 5, [255, 255, 255, 255]);
					}
				}
			}
		}
		for (y, row) in [2u8, 2, 7, 2, 2].iter().enumerate() {
			for x in 0..3 {
				if row & (1 << (2 - x)) != 0 {
					paint(10 + x, y + 5, [255, 255, 255, 255]);
				}
			}
		}
	}
	pixels
}
