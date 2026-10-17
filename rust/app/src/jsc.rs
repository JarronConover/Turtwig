/*! A minimal binding to Apple's JavaScriptCore (the JIT engine Safari uses, built into macOS): evaluate
source and register native functions that take and return strings. */

#![allow(non_camel_case_types)]

use std::ffi::{c_char, c_void, CString};

type JSContextRef = *const c_void;
type JSGlobalContextRef = *mut c_void;
type JSStringRef = *mut c_void;
type JSObjectRef = *mut c_void;
type JSValueRef = *const c_void;
type JSClassRef = *mut c_void;
type Callback = extern "C" fn(JSContextRef, JSObjectRef, JSObjectRef, usize, *const JSValueRef, *mut JSValueRef) -> JSValueRef;

#[link(name = "JavaScriptCore", kind = "framework")]
extern "C" {
    fn JSGlobalContextCreate(class: JSClassRef) -> JSGlobalContextRef;
    fn JSGlobalContextRelease(ctx: JSGlobalContextRef);
    fn JSContextGetGlobalObject(ctx: JSContextRef) -> JSObjectRef;
    fn JSStringCreateWithUTF8CString(s: *const c_char) -> JSStringRef;
    fn JSStringRelease(s: JSStringRef);
    fn JSStringGetMaximumUTF8CStringSize(s: JSStringRef) -> usize;
    fn JSStringGetUTF8CString(s: JSStringRef, buf: *mut c_char, size: usize) -> usize;
    fn JSEvaluateScript(ctx: JSContextRef, script: JSStringRef, this: JSObjectRef, url: JSStringRef, line: i32, exception: *mut JSValueRef) -> JSValueRef;
    fn JSObjectMakeFunctionWithCallback(ctx: JSContextRef, name: JSStringRef, cb: Callback) -> JSObjectRef;
    fn JSObjectSetProperty(ctx: JSContextRef, obj: JSObjectRef, name: JSStringRef, value: JSValueRef, attrs: u32, exception: *mut JSValueRef);
    fn JSObjectGetProperty(ctx: JSContextRef, obj: JSObjectRef, name: JSStringRef, exception: *mut JSValueRef) -> JSValueRef;
    fn JSObjectCallAsFunction(ctx: JSContextRef, f: JSObjectRef, this: JSObjectRef, argc: usize, argv: *const JSValueRef, exception: *mut JSValueRef) -> JSValueRef;
    fn JSValueToStringCopy(ctx: JSContextRef, v: JSValueRef, exception: *mut JSValueRef) -> JSStringRef;
    fn JSValueMakeString(ctx: JSContextRef, s: JSStringRef) -> JSValueRef;
    fn JSValueMakeUndefined(ctx: JSContextRef) -> JSValueRef;
    fn JSValueIsObject(ctx: JSContextRef, v: JSValueRef) -> bool;
}

struct JsString(JSStringRef);

impl JsString {
    fn new(s: &str) -> Self {
        let c = CString::new(s.replace('\0', "")).unwrap_or_default();
        JsString(unsafe { JSStringCreateWithUTF8CString(c.as_ptr()) })
    }
}

impl Drop for JsString {
    fn drop(&mut self) {
        unsafe { JSStringRelease(self.0) }
    }
}

fn to_rust(ctx: JSContextRef, v: JSValueRef) -> String {
    unsafe {
        let s = JSValueToStringCopy(ctx, v, std::ptr::null_mut());
        if s.is_null() {
            return String::new();
        }
        let size = JSStringGetMaximumUTF8CStringSize(s);
        let mut buf = vec![0u8; size.max(1)];
        let n = JSStringGetUTF8CString(s, buf.as_mut_ptr() as *mut c_char, size);
        JSStringRelease(s);
        buf.truncate(n.saturating_sub(1));
        String::from_utf8_lossy(&buf).into_owned()
    }
}

/** A native function: gets its arguments as strings, returns a string (or None for undefined). */
pub type Native = Box<dyn Fn(&[String]) -> Option<String>>;

thread_local! {
    static NATIVES: std::cell::RefCell<Vec<(String, Native)>> = const { std::cell::RefCell::new(vec![]) };
}

pub struct Engine {
    ctx: JSGlobalContextRef,
}

macro_rules! trampoline {
    ($($i:literal),*) => {
        [$({
            extern "C" fn t(ctx: JSContextRef, _: JSObjectRef, _: JSObjectRef, argc: usize, argv: *const JSValueRef, _: *mut JSValueRef) -> JSValueRef {
                let args: Vec<String> = (0..argc).map(|k| to_rust(ctx, unsafe { *argv.add(k) })).collect();
                let out = NATIVES.with(|n| n.borrow().get($i).and_then(|(_, f)| f(&args)));
                match out {
                    Some(s) => { let js = JsString::new(&s); unsafe { JSValueMakeString(ctx, js.0) } }
                    None => unsafe { JSValueMakeUndefined(ctx) },
                }
            }
            t as Callback
        }),*]
    };
}

const TRAMPOLINES: [Callback; 32] = trampoline!(0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31);

impl Engine {
    pub fn new() -> Self {
        NATIVES.with(|n| n.borrow_mut().clear());
        Engine { ctx: unsafe { JSGlobalContextCreate(std::ptr::null_mut()) } }
    }

    /** Defines a global function `name`; up to 32 per engine. */
    pub fn define(&mut self, name: &str, f: Native) {
        let ix = NATIVES.with(|n| {
            let mut n = n.borrow_mut();
            n.push((name.to_string(), f));
            n.len() - 1
        });
        let Some(cb) = TRAMPOLINES.get(ix) else { return };
        unsafe {
            let js_name = JsString::new(name);
            let func = JSObjectMakeFunctionWithCallback(self.ctx, js_name.0, *cb);
            JSObjectSetProperty(self.ctx, JSContextGetGlobalObject(self.ctx), js_name.0, func, 0, std::ptr::null_mut());
        }
    }

    /** Runs source; Err holds the exception text. */
    pub fn eval(&mut self, source: &str, name: &str) -> Result<(), String> {
        let mut exc: JSValueRef = std::ptr::null();
        unsafe {
            let src = JsString::new(source);
            let url = JsString::new(name);
            JSEvaluateScript(self.ctx, src.0, std::ptr::null_mut(), url.0, 1, &mut exc);
        }
        if exc.is_null() {
            Ok(())
        } else {
            Err(to_rust(self.ctx, exc))
        }
    }

    /** Calls global function `name` with one string argument. */
    pub fn call(&mut self, name: &str, arg: &str) -> Result<(), String> {
        let mut exc: JSValueRef = std::ptr::null();
        unsafe {
            let n = JsString::new(name);
            let f = JSObjectGetProperty(self.ctx, JSContextGetGlobalObject(self.ctx), n.0, &mut exc);
            if !exc.is_null() || !JSValueIsObject(self.ctx, f) {
                return Err(format!("no function {name}"));
            }
            let a = JsString::new(arg);
            let argv = [JSValueMakeString(self.ctx, a.0)];
            JSObjectCallAsFunction(self.ctx, f as JSObjectRef, std::ptr::null_mut(), 1, argv.as_ptr(), &mut exc);
        }
        if exc.is_null() {
            Ok(())
        } else {
            Err(to_rust(self.ctx, exc))
        }
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        unsafe { JSGlobalContextRelease(self.ctx) }
    }
}
