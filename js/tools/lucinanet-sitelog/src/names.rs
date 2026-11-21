//! Which side of the boundary a missing name belongs to: the language engine's or the web engine's.
//!
//! The rule is a name list, not a spec database (the README states it where the tables read it): a name
//! ECMAScript or ECMA-402 defines is `engine`'s to provide, a name the DOM and the rest of the Web
//! platform define is `web-engine`'s, and a name on neither list follows the channel it arrived on.

/// Globals and intrinsics of ECMA-262, plus the ECMA-402 constructors and functions.
const ECMASCRIPT: &[&str] = &[
    "AggregateError", "Array", "ArrayBuffer", "AsyncDisposableStack", "AsyncFunction", "AsyncGenerator", "AsyncGeneratorFunction", "AsyncIterator",
    "Atomics", "BigInt", "BigInt64Array", "BigUint64Array", "Boolean", "DataView", "Date", "DisposableStack", "Error", "EvalError",
    "FinalizationRegistry", "Float16Array", "Float32Array", "Float64Array", "Function", "Generator", "GeneratorFunction", "Infinity", "InternalError",
    "Int8Array", "Int16Array", "Int32Array", "Intl", "JSON", "Map", "Math", "NaN", "Number", "Object", "Promise", "Proxy", "RangeError",
    "ReferenceError", "Reflect", "RegExp", "Set", "SharedArrayBuffer", "String", "SuppressedError", "Symbol", "SyntaxError", "Temporal", "TypeError",
    "URIError", "Uint8Array", "Uint8ClampedArray", "Uint16Array", "Uint32Array", "WeakMap", "WeakRef", "WeakSet",
    "decodeURI", "decodeURIComponent", "encodeURI", "encodeURIComponent", "escape", "eval", "globalThis", "isFinite", "isNaN", "parseFloat",
    "parseInt", "undefined",
    // ECMA-402 namespace members, spelled both ways a message can name them.
    "Collator", "DateTimeFormat", "DisplayNames", "DurationFormat", "ListFormat", "Locale", "NumberFormat", "PluralRules", "RelativeTimeFormat",
    "Segmenter", "getCanonicalLocales", "supportedValuesOf",
];

/// Globals the Web platform defines: DOM, HTML, URL, File, Fetch, Encoding, WebSockets and friends.
const WEB: &[&str] = &[
    "AbortController", "AbortSignal", "Audio", "Blob", "BroadcastChannel", "CSS", "CanvasRenderingContext2D", "Clipboard", "CloseEvent", "CustomEvent",
    "DOMException", "DOMParser", "DataTransfer", "Document", "Element", "Event", "EventSource", "EventTarget", "File", "FileList", "FileReader",
    "FormData", "Geolocation", "HTMLDivElement", "HTMLDocument", "HTMLElement", "Headers", "History", "IDBKeyRange", "Image", "ImageData",
    "IntersectionObserver", "Location", "MessageChannel", "MessageEvent", "MessagePort", "MutationObserver", "Node", "NodeList", "Notification",
    "OffscreenCanvas", "Performance", "PerformanceObserver", "PushManager", "RTCPeerConnection", "ReadableStream", "Request", "ResizeObserver",
    "Response", "Screen", "ServiceWorker", "ServiceWorkerRegistration", "ShadowRoot", "SharedWorker", "Storage", "TextDecoder", "TextEncoder",
    "TransformStream", "URL", "URLPattern", "URLSearchParams", "WGSLLanguageFeatures", "WebSocket", "Worker", "WritableStream", "XMLDocument",
    "XMLHttpRequest", "XMLSerializer", "XPathResult", "alert", "atob", "btoa", "caches", "cancelAnimationFrame", "clearInterval", "clearTimeout",
    "confirm", "createImageBitmap", "crypto", "customElements", "document", "fetch", "getComputedStyle", "history", "indexedDB", "innerHeight",
    "innerWidth", "localStorage", "location", "matchMedia", "navigator", "parent", "performance", "postMessage", "prompt", "queueMicrotask",
    "requestAnimationFrame", "screen", "self", "sessionStorage", "setTimeout", "setInterval", "structuredClone", "top", "window",
];

/// Whether `name` is one of the listed globals, allowing `Intl.X` to match on `X` as well.
fn in_list(list: &[&str], name: &str) -> bool {
    list.contains(&name) || name.split_once('.').is_some_and(|(_, tail)| list.contains(&tail))
}

/// Whose job `name` is: `engine` for a standard ECMAScript or ECMA-402 name, `web-engine` for a Web
/// platform one. A name on neither list is the engine's when `stub` — the row came from a
/// `not implemented` line, which only the glue writes — and the web engine's otherwise, because what a
/// page reaches for and does not find is nearly always a Web global the browser has not wired yet.
pub fn owner(name: &str, stub: bool) -> &'static str {
    if in_list(ECMASCRIPT, name) {
        "engine"
    } else if in_list(WEB, name) {
        "web-engine"
    } else if stub {
        "engine"
    } else {
        "web-engine"
    }
}

/// The wordings that say something is missing. Each is the tail of a message whose head names the thing.
const PHRASES: &[&str] = &[" is not defined", " is not a constructor", " is not a function", " is not implemented", " not supported in this build"];

/// `true` when the message says something is missing, whatever it then names.
pub fn names_a_gap(message: &str) -> bool {
    PHRASES.iter().any(|p| message.contains(p))
}

/// The name a message says is missing: the last word before the earliest of the `PHRASES`, with the
/// error type (`ReferenceError: `) and a `new ` or `Uncaught ` dropped. `None` when the message names
/// nothing, or names something that does not read as a name.
pub fn missing_name(message: &str) -> Option<&str> {
    let at = PHRASES.iter().filter_map(|p| message.find(p)).min()?;
    let head = &message[..at];
    let head = head.strip_prefix("Uncaught ").unwrap_or(head);
    // `ReferenceError: Intl is not defined` names Intl, not ReferenceError.
    let head = match head.split_once(": ") {
        Some((prefix, rest)) if prefix.ends_with("Error") && !rest.is_empty() => rest,
        _ => head,
    };
    // `not supported in this build` leaves its copula behind; the other phrases include it.
    let head = head.trim_end().strip_suffix(" is").unwrap_or(head).trim_end();
    let head = head.strip_prefix("new ").unwrap_or(head);
    let word = head.split_whitespace().next_back()?.trim_end_matches(|c: char| matches!(c, '.' | ',' | ':' | ';' | ')' | ']' | '}'));
    let word = word.strip_prefix('(').unwrap_or(word);
    if !word.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$') {
        return None;
    }
    if !word.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '.' | ':' | '#')) {
        return None;
    }
    Some(word)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_names_are_the_engine_and_web_names_are_not() {
        assert_eq!(owner("Intl", false), "engine");
        assert_eq!(owner("DurationFormat", false), "engine");
        assert_eq!(owner("Intl.DurationFormat", true), "engine");
        assert_eq!(owner("Int8Array", false), "engine");
        assert_eq!(owner("WeakRef", false), "engine");
        assert_eq!(owner("IntersectionObserver", false), "web-engine");
        assert_eq!(owner("document", false), "web-engine");
        assert_eq!(owner("structuredClone", false), "web-engine");
    }

    #[test]
    fn an_unknown_name_follows_the_channel_it_came_on() {
        assert_eq!(owner("glue::NewExternalArrayBuffer", true), "engine");
        assert_eq!(owner("SomePageGlobal", false), "web-engine");
    }

    #[test]
    fn the_missing_name_is_the_word_before_the_phrase() {
        assert_eq!(missing_name("ReferenceError: Intl is not defined"), Some("Intl"));
        assert_eq!(missing_name("TypeError: IntersectionObserver is not a constructor"), Some("IntersectionObserver"));
        assert_eq!(missing_name("Uncaught ReferenceError: foo is not defined"), Some("foo"));
        assert_eq!(missing_name("this.state.format is not a function"), Some("this.state.format"));
        assert_eq!(missing_name("WebAssembly is not supported in this build"), Some("WebAssembly"));
        assert_eq!(missing_name("new Promise is not a function"), Some("Promise"));
        assert_eq!(missing_name("glue::NewExternalArrayBuffer is not implemented"), Some("glue::NewExternalArrayBuffer"));
    }

    #[test]
    fn a_message_that_names_nothing_has_no_name() {
        assert_eq!(missing_name("TypeError: cannot set property stack"), None);
        assert_eq!(missing_name("No valid entry type provided to observe()."), None);
        assert_eq!(missing_name("TypeError: X is not defined for 3 things"), Some("X"));
        assert!(names_a_gap("Intl is not defined"));
        assert!(!names_a_gap("Unhandled promise rejection: TypeError: nope"));
    }
}
