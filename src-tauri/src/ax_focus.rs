// macOS Accessibility API bindings for focus/role inspection.
//
// Extracted from clipboard.rs so the delayed-paste watcher and the paste
// verifier can share the same raw FFI surface without growing clipboard.rs.

#[cfg(target_os = "macos")]
use std::os::raw::c_int;

// Opaque types from ApplicationServices.framework
#[repr(C)]
pub struct AXUIElement(pub *mut std::ffi::c_void);
#[repr(C)]
pub struct CFString(pub *mut std::ffi::c_void);
#[repr(C)]
pub struct CFType(pub *mut std::ffi::c_void);

pub type AXUIElementRef = *const AXUIElement;
pub type CFTypeRef = *const CFType;
pub type CFStringRef = *const CFString;
pub type AXError = c_int;
pub type CFStringEncoding = u32;

// AXError codes
pub const KAX_ERROR_SUCCESS: i32 = 0;
/// kAXErrorCannotComplete — with AXUIElementSetMessagingTimeout in place,
/// this is what a HUNG target app returns (the AX message timed out).
/// It means the app is unresponsive, NOT that an attribute is missing,
/// so it must not be treated as "terminal-like app, paste landed".
pub const KAX_ERROR_CANNOT_COMPLETE: i32 = -25204;
/// kAXErrorAttributeUnsupported — the element genuinely does not support
/// the attribute (many terminal apps don't expose AXValue).
pub const KAX_ERROR_ATTRIBUTE_UNSUPPORTED: i32 = -25205;
/// kAXErrorNoValue — the attribute exists but has no value.
pub const KAX_ERROR_NO_VALUE: i32 = -25212;

// CoreFoundation encoding
pub const K_CFSTRING_ENCODING_UTF8: u32 = 0x08000100;

/// Per-element AX messaging timeout. 1.0s is generous for a healthy app;
/// a hung target app now degrades (verification fails → conservative
/// keep-on-clipboard) instead of parking the caller forever.
const AX_MESSAGING_TIMEOUT_SECS: f32 = 1.0;

/// Maximum number of descents below the app-level focused element while
/// following AXFocusedUIElement links (chain length ≤ 6 including the
/// app-level element). Browsers and Electron apps nest the real text field
/// several levels below generic wrappers.
const MAX_AX_FOCUS_DEPTH: usize = 5;

/// Roles that are directly editable text fields; encountering one ends the
/// focus-chain walk early (its AXValue is the pending-paste text target).
/// Mirrors `EDITABLE_ROLES` in pending_paste.rs.
const EDITABLE_AX_ROLES: [&str; 4] = ["AXTextArea", "AXTextField", "AXComboBox", "AXSearchField"];

/// Information about the currently focused accessibility element.
pub struct FocusedElementInfo {
    /// AXRoles from the app-level focused element down to the deepest
    /// element reached, in descent order.
    pub role_chain: Vec<String>,
    /// Why the bounded focus-chain walk stopped.
    pub chain_end: FocusChainEnd,
    /// AXValue of the deepest role-identified element, when requested and
    /// readable.
    pub value: Option<String>,
}

/// Why the bounded AXFocusedUIElement walk stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusChainEnd {
    /// The deepest element exposes no distinct, not-already-visited
    /// AXFocusedUIElement (read error, null, self-equal, or cycle).
    NoDeeperFocus,
    /// AXRole of the deepest element was unreadable.
    UnreadableRole,
    /// A distinct child existed when the depth cap was hit (likely a cycle).
    DepthCap,
}

/// Create a CFString for the `AXFocusedUIElement` accessibility attribute.
/// Caller must release the returned CFStringRef with CFRelease.
pub fn ax_focused_ui_element_attribute() -> CFStringRef {
    unsafe {
        CFStringCreateWithCString(
            std::ptr::null::<std::ffi::c_void>() as CFTypeRef,
            c"AXFocusedUIElement".as_ptr(),
            K_CFSTRING_ENCODING_UTF8,
        )
    }
}

/// Create a CFString for the `AXRole` accessibility attribute.
/// Caller must release the returned CFStringRef with CFRelease.
pub fn ax_role_attribute() -> CFStringRef {
    unsafe {
        CFStringCreateWithCString(
            std::ptr::null::<std::ffi::c_void>() as CFTypeRef,
            c"AXRole".as_ptr(),
            K_CFSTRING_ENCODING_UTF8,
        )
    }
}

/// Create a CFString for the `AXValue` accessibility attribute.
/// Caller must release the returned CFStringRef with CFRelease.
pub fn ax_value_attribute() -> CFStringRef {
    unsafe {
        CFStringCreateWithCString(
            std::ptr::null::<std::ffi::c_void>() as CFTypeRef,
            c"AXValue".as_ptr(),
            K_CFSTRING_ENCODING_UTF8,
        )
    }
}

#[allow(clashing_extern_declarations)]
#[link(kind = "framework", name = "ApplicationServices")]
extern "C" {
    pub fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    // Bounds the round-trip time of AX messages sent to `element`'s
    // process (seconds, f32 per HIServices). Without this,
    // AXUIElementCopyAttributeValue against a hung target app blocks the
    // calling thread FOREVER — observed live: the macOS main thread parked
    // permanently inside the paste closure and wedged the whole app (see
    // research/main-thread-hang-tray-lock-wedge-2026-10-23.md).
    pub fn AXUIElementSetMessagingTimeout(
        element: AXUIElementRef,
        inTimeoutSeconds: f32,
    ) -> AXError;
    pub fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        // `attribute` is a CFStringRef (a CoreFoundation object pointer), NOT
        // a raw C string. Passing a *const i8 here previously made the framework
        // call CFStringGetLength on a non-CFString pointer, triggering an Apple
        // Silicon PAC trap (SIGTRAP / EXC_BREAKPOINT) and crashing the app
        // after every successful paste. We now build proper CFString objects
        // with CFStringCreateWithCString instead of relying on linkable globals.
        attribute: CFStringRef,
        value: *mut *mut std::ffi::c_void,
    ) -> AXError;
    pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
    pub fn CFStringGetTypeID() -> usize;
    pub fn CFStringGetLength(theString: CFStringRef) -> isize;
    pub fn CFStringGetCString(
        theString: CFStringRef,
        buffer: *mut i8,
        bufferSize: isize,
        encoding: CFStringEncoding,
    ) -> bool;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFStringCreateWithCString(
        alloc: CFTypeRef,
        cStr: *const i8,
        encoding: CFStringEncoding,
    ) -> CFStringRef;
    /// Semantic equality of two CoreFoundation objects (C Boolean: nonzero
    /// = equal). For AXUIElementRef this dispatches to the equality callback
    /// HIServices registers for the AXUIElement type, i.e. semantic element
    /// equality. Used as the descent-loop guard so cyclic
    /// AXFocusedUIElement chains terminate instead of looping forever.
    /// (AXUIElementEqual itself is not exported by modern macOS HIServices;
    /// CFEqual is the documented polymorphic equivalent.)
    pub fn CFEqual(cf1: CFTypeRef, cf2: CFTypeRef) -> bool;
}

/// Read the focused element of the given application by walking the
/// AXFocusedUIElement chain and collect the roles of every element visited.
///
/// Web content and Electron apps expose the real focused node (a text input,
/// etc.) one or more AXFocusedUIElement links below generic wrappers such as
/// AXWebArea/AXGroup, so a single-level read misses it. This walk descends up
/// to MAX_AX_FOCUS_DEPTH levels, guards against cycles with CFEqual,
/// and stops early at an editable role.
///
/// ANY AX failure stops the walk; the collected chain plus `chain_end`
/// describe where it stopped. Callers should treat chains ending in
/// `UnreadableRole` or `DepthCap` as Unknown so they fall back to
/// conservative behavior rather than guessing.
pub fn read_focused_element(pid: i32, include_value: bool) -> Option<FocusedElementInfo> {
    use objc2::rc::autoreleasepool;

    autoreleasepool(|_| {
        // Use the macOS Accessibility API to check the focused element.
        // This works at the window-server level, independent of whether
        // our panel is click-through.
        let ax_app: AXUIElementRef = unsafe { AXUIElementCreateApplication(pid) };

        // Bound AX messages against the target app process — without this an
        // unbounded AXUIElementCopyAttributeValue can park this thread forever.
        let _ = unsafe { AXUIElementSetMessagingTimeout(ax_app, AX_MESSAGING_TIMEOUT_SECS) };

        // Build the AX attribute name CFStrings at runtime. Modern macOS SDKs
        // no longer export linkable globals for these (they are CFSTR macros
        // in headers), so we create them from C-string literals and release
        // them once the read is done.
        let focused_attr = ax_focused_ui_element_attribute();
        let role_attr = ax_role_attribute();
        let value_attr = ax_value_attribute();

        let mut focused_element_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
        let result = unsafe {
            AXUIElementCopyAttributeValue(ax_app, focused_attr, &mut focused_element_ptr as *mut _)
        };

        if result != KAX_ERROR_SUCCESS {
            unsafe {
                // Release attribute CFStrings and ax_app (Create Rule: caller must release).
                CFRelease(focused_attr as CFTypeRef);
                CFRelease(role_attr as CFTypeRef);
                CFRelease(value_attr as CFTypeRef);
                CFRelease(ax_app as CFTypeRef);
            }
            return None;
        }

        let focused_element: AXUIElementRef = focused_element_ptr as AXUIElementRef;

        // The focused element lives in the (possibly hung) target process too —
        // bound it with the same messaging timeout.
        let _ =
            unsafe { AXUIElementSetMessagingTimeout(focused_element, AX_MESSAGING_TIMEOUT_SECS) };

        // Bounded walk down the AXFocusedUIElement chain. Every element we
        // take ownership of goes into `visited` and is released exactly once
        // at function exit.
        let mut visited: Vec<AXUIElementRef> = vec![focused_element];
        let mut current = focused_element;
        let mut role_chain: Vec<String> = Vec::new();
        let mut value: Option<String> = None;
        let mut chain_end = FocusChainEnd::NoDeeperFocus;

        // At most MAX_AX_FOCUS_DEPTH descents below the app-level focused
        // element, i.e. at most MAX_AX_FOCUS_DEPTH + 1 role reads.
        for _ in 0..=MAX_AX_FOCUS_DEPTH {
            // Each iteration terminates the walk here unless a distinct
            // child is descended into at the bottom.
            chain_end = FocusChainEnd::NoDeeperFocus;

            let Some(role) = read_string_attribute(current, role_attr) else {
                chain_end = FocusChainEnd::UnreadableRole;
                break;
            };
            role_chain.push(role.clone());

            // Editable leaf: this element is the paste target; stop early.
            if EDITABLE_AX_ROLES.contains(&role.as_str()) {
                if include_value {
                    value = read_string_attribute(current, value_attr);
                }
                break;
            }

            // Non-editable: try to descend into its focused child (web
            // content nests the real focus under AXWebArea/AXGroup wrappers).
            let Some(child) = read_focused_child(current, focused_attr) else {
                if include_value {
                    value = read_string_attribute(current, value_attr);
                }
                break;
            };

            // Children live in the target process too — bound their messages.
            let _ = unsafe { AXUIElementSetMessagingTimeout(child, AX_MESSAGING_TIMEOUT_SECS) };

            // Cycle guard: some trees expose an AXFocusedUIElement that
            // points back at an ancestor (or itself). Compare by pointer and
            // by CFEqual (semantic AX element equality) before keeping the
            // child.
            let revisits_chain = child == current
                || visited.iter().any(|visited_element| unsafe {
                    CFEqual(child as CFTypeRef, *visited_element as CFTypeRef)
                });
            if revisits_chain {
                unsafe { CFRelease(child as CFTypeRef) };
                if include_value {
                    value = read_string_attribute(current, value_attr);
                }
                break;
            }

            visited.push(child);
            current = child;
            // A distinct child is pending; if the loop exhausts here the
            // chain is deeper than the cap (likely a cycle).
            chain_end = FocusChainEnd::DepthCap;
        }

        unsafe {
            // Release every visited element (Create-rule +1 references), the
            // attribute CFStrings, and ax_app.
            for element in visited {
                CFRelease(element as CFTypeRef);
            }
            CFRelease(focused_attr as CFTypeRef);
            CFRelease(role_attr as CFTypeRef);
            CFRelease(value_attr as CFTypeRef);
            CFRelease(ax_app as CFTypeRef);
        }

        Some(FocusedElementInfo {
            role_chain,
            chain_end,
            value,
        })
    })
}

/// Read the AXFocusedUIElement attribute of `element`.
///
/// Returns the child element (Create Rule: caller owns a +1 reference and
/// must CFRelease it) or None if the read failed or the value is null.
fn read_focused_child(
    element: AXUIElementRef,
    focused_attr: CFStringRef,
) -> Option<AXUIElementRef> {
    let mut child_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let result =
        unsafe { AXUIElementCopyAttributeValue(element, focused_attr, &mut child_ptr as *mut _) };

    if result != KAX_ERROR_SUCCESS || child_ptr.is_null() {
        return None;
    }

    Some(child_ptr as AXUIElementRef)
}

/// Read a string-valued AX attribute. Returns None if the attribute is
/// missing, unsupported, has no value, or is not a CFString.
fn read_string_attribute(element: AXUIElementRef, attribute: CFStringRef) -> Option<String> {
    let mut value_ptr: *mut std::ffi::c_void = std::ptr::null_mut();
    let value_result =
        unsafe { AXUIElementCopyAttributeValue(element, attribute, &mut value_ptr as *mut _) };

    if value_result != KAX_ERROR_SUCCESS {
        return None;
    }

    let type_id = unsafe { CFGetTypeID(value_ptr as CFTypeRef) };
    let string_type_id = unsafe { CFStringGetTypeID() };

    if type_id != string_type_id {
        unsafe { CFRelease(value_ptr as CFTypeRef) };
        return None;
    }

    let cf_string: CFStringRef = value_ptr as CFStringRef;
    let len = unsafe { CFStringGetLength(cf_string) };
    // Allocate buffer for UTF-8 string + null terminator.
    // Each UTF-16 code unit can expand to up to 3 UTF-8 bytes.
    let buffer_size = (len as usize) * 3 + 1;
    let mut buffer = vec![0u8; buffer_size];

    let success = unsafe {
        CFStringGetCString(
            cf_string,
            buffer.as_mut_ptr() as *mut i8,
            buffer_size as isize,
            K_CFSTRING_ENCODING_UTF8,
        )
    };

    let result = if success {
        let end = buffer.iter().position(|&b| b == 0).unwrap_or(buffer.len());
        let s = String::from_utf8_lossy(&buffer[..end]).to_string();
        Some(s)
    } else {
        None
    };

    // Release the CFString object obtained from AXUIElementCopyAttributeValue
    // (Core Foundation Create Rule: caller must release).
    unsafe { CFRelease(value_ptr as CFTypeRef) };

    result
}
