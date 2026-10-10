import Carbon.HIToolbox
import CoreGraphics
import Foundation

struct DesktopFailure: Error {
    let code: String
    let message: String
}

enum DesktopInput {
    static let keys: [String: Int] = [
        "a": kVK_ANSI_A,
        "s": kVK_ANSI_S,
        "d": kVK_ANSI_D,
        "f": kVK_ANSI_F,
        "h": kVK_ANSI_H,
        "g": kVK_ANSI_G,
        "z": kVK_ANSI_Z,
        "x": kVK_ANSI_X,
        "c": kVK_ANSI_C,
        "v": kVK_ANSI_V,
        "b": kVK_ANSI_B,
        "q": kVK_ANSI_Q,
        "w": kVK_ANSI_W,
        "e": kVK_ANSI_E,
        "r": kVK_ANSI_R,
        "y": kVK_ANSI_Y,
        "t": kVK_ANSI_T,
        "1": kVK_ANSI_1,
        "2": kVK_ANSI_2,
        "3": kVK_ANSI_3,
        "4": kVK_ANSI_4,
        "6": kVK_ANSI_6,
        "5": kVK_ANSI_5,
        "equal": kVK_ANSI_Equal,
        "9": kVK_ANSI_9,
        "7": kVK_ANSI_7,
        "minus": kVK_ANSI_Minus,
        "8": kVK_ANSI_8,
        "0": kVK_ANSI_0,
        "rightbracket": kVK_ANSI_RightBracket,
        "o": kVK_ANSI_O,
        "u": kVK_ANSI_U,
        "leftbracket": kVK_ANSI_LeftBracket,
        "i": kVK_ANSI_I,
        "p": kVK_ANSI_P,
        "l": kVK_ANSI_L,
        "j": kVK_ANSI_J,
        "quote": kVK_ANSI_Quote,
        "k": kVK_ANSI_K,
        "semicolon": kVK_ANSI_Semicolon,
        "backslash": kVK_ANSI_Backslash,
        "comma": kVK_ANSI_Comma,
        "slash": kVK_ANSI_Slash,
        "n": kVK_ANSI_N,
        "m": kVK_ANSI_M,
        "period": kVK_ANSI_Period,
        "grave": kVK_ANSI_Grave,
        "keypaddecimal": kVK_ANSI_KeypadDecimal,
        "keypadmultiply": kVK_ANSI_KeypadMultiply,
        "keypadplus": kVK_ANSI_KeypadPlus,
        "keypadclear": kVK_ANSI_KeypadClear,
        "keypaddivide": kVK_ANSI_KeypadDivide,
        "keypadenter": kVK_ANSI_KeypadEnter,
        "keypadminus": kVK_ANSI_KeypadMinus,
        "keypadequals": kVK_ANSI_KeypadEquals,
        "keypad0": kVK_ANSI_Keypad0,
        "keypad1": kVK_ANSI_Keypad1,
        "keypad2": kVK_ANSI_Keypad2,
        "keypad3": kVK_ANSI_Keypad3,
        "keypad4": kVK_ANSI_Keypad4,
        "keypad5": kVK_ANSI_Keypad5,
        "keypad6": kVK_ANSI_Keypad6,
        "keypad7": kVK_ANSI_Keypad7,
        "keypad8": kVK_ANSI_Keypad8,
        "keypad9": kVK_ANSI_Keypad9,
        "enter": kVK_Return,
        "tab": kVK_Tab,
        "space": kVK_Space,
        "backspace": kVK_Delete,
        "escape": kVK_Escape,
        "cmd": kVK_Command,
        "shift": kVK_Shift,
        "capslock": kVK_CapsLock,
        "alt": kVK_Option,
        "ctrl": kVK_Control,
        "rightcmd": kVK_RightCommand,
        "rightshift": kVK_RightShift,
        "rightalt": kVK_RightOption,
        "rightctrl": kVK_RightControl,
        "fn": kVK_Function,
        "f17": kVK_F17,
        "volumeup": kVK_VolumeUp,
        "volumedown": kVK_VolumeDown,
        "mute": kVK_Mute,
        "f18": kVK_F18,
        "f19": kVK_F19,
        "f20": kVK_F20,
        "f5": kVK_F5,
        "f6": kVK_F6,
        "f7": kVK_F7,
        "f3": kVK_F3,
        "f8": kVK_F8,
        "f9": kVK_F9,
        "f11": kVK_F11,
        "f13": kVK_F13,
        "f16": kVK_F16,
        "f14": kVK_F14,
        "f10": kVK_F10,
        "contextualmenu": kVK_ContextualMenu,
        "f12": kVK_F12,
        "f15": kVK_F15,
        "help": kVK_Help,
        "home": kVK_Home,
        "pageup": kVK_PageUp,
        "delete": kVK_ForwardDelete,
        "f4": kVK_F4,
        "end": kVK_End,
        "f2": kVK_F2,
        "pagedown": kVK_PageDown,
        "f1": kVK_F1,
        "left": kVK_LeftArrow,
        "right": kVK_RightArrow,
        "down": kVK_DownArrow,
        "up": kVK_UpArrow,
        "isosection": kVK_ISO_Section,
        "jisyen": kVK_JIS_Yen,
        "jisunderscore": kVK_JIS_Underscore,
        "jiskeypadcomma": kVK_JIS_KeypadComma,
        "jiseisu": kVK_JIS_Eisu,
        "jiskana": kVK_JIS_Kana
    ]

    static func key(_ value: String) throws -> (CGKeyCode, CGEventFlags) {
        let parts = value.lowercased().split(separator: "+", omittingEmptySubsequences: false).map(String.init)
        guard let key = parts.last, let code = keys[key], Set(parts.dropLast()).count == parts.count - 1 else {
            throw DesktopFailure(code: "unsupported_key", message: "Use one declared physical key with unique cmd/ctrl/alt/shift/fn modifiers")
        }
        var flags: CGEventFlags = []
        for modifier in parts.dropLast() {
            switch modifier {
            case "cmd": flags.insert(.maskCommand)
            case "ctrl": flags.insert(.maskControl)
            case "alt": flags.insert(.maskAlternate)
            case "shift": flags.insert(.maskShift)
            case "fn": flags.insert(.maskSecondaryFn)
            default: throw DesktopFailure(code: "unsupported_modifier", message: "Use cmd, ctrl, alt, shift or fn")
            }
        }
        return (CGKeyCode(code), flags)
    }

    static func click(_ action: [String: Any]) throws -> (CGMouseButton, CGEventType, CGEventType, Int) {
        let button: String
        if let value = action["button"] {
            guard let value = value as? String else { throw DesktopFailure(code: "invalid_button", message: "Button must be left, right or middle") }
            button = value
        } else { button = "left" }
        let count: Int
        if let value = action["count"] {
            guard let value = value as? NSNumber, CFGetTypeID(value) != CFBooleanGetTypeID(), value.doubleValue == 1 || value.doubleValue == 2 else { throw DesktopFailure(code: "invalid_click_count", message: "Click count must be integer 1 or 2") }
            count = value.intValue
        } else { count = 1 }
        switch button {
        case "left": return (.left, .leftMouseDown, .leftMouseUp, count)
        case "right": return (.right, .rightMouseDown, .rightMouseUp, count)
        case "middle": return (.center, .otherMouseDown, .otherMouseUp, count)
        default: throw DesktopFailure(code: "invalid_button", message: "Button must be left, right or middle")
        }
    }
}
