import AppKit
import ApplicationServices
import CoreText
import Foundation

struct DesktopAXTextPresentation: Codable, Equatable {
    let source: String
    let format: String
    let text: String
}

struct DesktopAXMappedText: Equatable {
    private(set) var text: String
    private(set) var offsets: [Int?]
    static let maximumBytes = 4 * 1024 * 1024

    init(_ text: String = "", sourceStart: Int? = nil) throws {
        guard text.utf8.count <= Self.maximumBytes, sourceStart.map({ $0 >= 0 && $0 <= Int.max - text.utf16.count }) ?? true else {
            throw DesktopFailure(code: "ax_presentation_limit", message: "Accessibility text exceeds its bounded source-map domain")
        }
        self.text = text
        offsets = (0..<text.utf16.count).map { index in sourceStart.map { $0 + index } }
    }

    private init(text: String, offsets: [Int?]) throws {
        guard text.utf8.count <= Self.maximumBytes, text.utf16.count == offsets.count else {
            throw DesktopFailure(code: "invalid_ax_text_map", message: "Rendered UTF-16 units do not match their retained source map")
        }
        self.text = text; self.offsets = offsets
    }

    mutating func append(_ other: Self) throws {
        guard other.text.utf8.count <= Self.maximumBytes - text.utf8.count else {
            throw DesktopFailure(code: "ax_presentation_limit", message: "Accessibility presentation exceeds its byte bound")
        }
        text += other.text; offsets += other.offsets
    }

    mutating func syntax(_ text: String) throws { try append(Self(text)) }

    func slice(_ range: NSRange) throws -> Self {
        guard range.location != NSNotFound, range.location >= 0, range.length >= 0,
              range.location <= offsets.count, range.length <= offsets.count - range.location else {
            throw DesktopFailure(code: "invalid_ax_text_map", message: "Rendered text slice is outside its exact UTF-16 map")
        }
        return try Self(text: (text as NSString).substring(with: range), offsets: Array(offsets[range.location..<NSMaxRange(range)]))
    }

    func trimmed() throws -> Self {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return try Self() }
        let range = (text as NSString).range(of: trimmed)
        guard range.location != NSNotFound, range.length == trimmed.utf16.count else { throw DesktopFailure(code: "invalid_ax_text_map", message: "Native trimmed text has no exact retained UTF-16 range") }
        return try slice(range)
    }

    func replacingNewlines(with replacement: Self) throws -> Self {
        let source = text as NSString
        var start = 0
        var result = try Self()
        while start < source.length {
            let range = source.range(of: "\n", options: [], range: NSRange(location: start, length: source.length - start))
            if range.location == NSNotFound { try result.append(slice(NSRange(location: start, length: source.length - start))); break }
            try result.append(slice(NSRange(location: start, length: range.location - start)))
            try result.append(replacement)
            start = NSMaxRange(range)
        }
        return result
    }

    func sourceRange(for range: NSRange) -> NSRange? {
        guard range.location != NSNotFound, range.location >= 0, range.length > 0,
              range.location <= offsets.count, range.length <= offsets.count - range.location else { return nil }
        let native = offsets[range.location..<NSMaxRange(range)].compactMap { $0 }
        guard let first = native.first, let last = native.last, last >= first else { return nil }
        return NSRange(location: first, length: last - first + 1)
    }

    static func uniqueRange(in value: String, text: String, prefix: String?, suffix: String?) -> NSRange? {
        let source = value as NSString
        guard !text.isEmpty, text.utf16.count <= 65536, (prefix?.utf16.count ?? 0) <= 65536, (suffix?.utf16.count ?? 0) <= 65536 else { return nil }
        var matches: NSRange?
        var start = 0
        while start <= source.length {
            let range = source.range(of: text, options: [], range: NSRange(location: start, length: source.length - start))
            guard range.location != NSNotFound else { break }
            let before = prefix.map { prefix in
                let length = (prefix as NSString).length
                return range.location >= length && source.substring(with: NSRange(location: range.location - length, length: length)) == prefix
            } ?? true
            let after = suffix.map { suffix in
                let length = (suffix as NSString).length
                return source.length - NSMaxRange(range) >= length && source.substring(with: NSRange(location: NSMaxRange(range), length: length)) == suffix
            } ?? true
            if before && after {
                guard matches == nil else { return nil }
                matches = range
            }
            start = range.location + 1
        }
        return matches
    }

    func sourceRange(text: String, prefix: String?, suffix: String?) -> NSRange? {
        guard let match = Self.uniqueRange(in: self.text, text: text, prefix: prefix, suffix: suffix) else { return nil }
        return sourceRange(for: match)
    }
}

struct DesktopAXURLRendering {
    var includeQuery = true
    var includeFragment = true
    var individualLimit: Int?
    var totalLimit: Int?
    private(set) var total = 0
    private var cache: [URL: String] = [:]

    mutating func reset() { total = 0; cache.removeAll(keepingCapacity: true) }

    mutating func compact() -> Bool {
        if includeFragment { includeFragment = false; return true }
        if includeQuery { includeQuery = false; return true }
        if individualLimit == nil { individualLimit = 120; return true }
        if let limit = individualLimit, limit > 20 { individualLimit = limit - 10; return true }
        return false
    }

    mutating func render(_ url: URL, isImage: Bool = false) -> String? {
        guard !isImage, let scheme = url.scheme?.lowercased(), !["data", "webdoc"].contains(scheme) else { return nil }
        if url.isFileURL { return url.absoluteString }
        if let existing = cache[url] { return existing }
        guard var parts = URLComponents(url: url, resolvingAgainstBaseURL: false) else { return nil }
        if parts.scheme == "http" || parts.scheme == "https" { parts.scheme = nil }
        if parts.host?.hasPrefix("www.") == true { parts.host = parts.host.map { String($0.dropFirst(4)) } }
        if !includeQuery { parts.query = nil }
        if !includeFragment { parts.fragment = nil }
        guard var text = parts.string else { return nil }
        if text.hasPrefix("//") { text.removeFirst(2) }
        if let limit = individualLimit, text.count > limit + 1 { text = String(text.prefix(limit - 1)) + "…" }
        if let limit = totalLimit, text.count > limit - min(limit, total) { text = "…" }
        total += text.count; cache[url] = text
        return text
    }
}

struct DesktopAXAttributedSource {
    let attributed: NSAttributedString
    let range: NSRange
    let source: String
    let mapped: DesktopAXMappedText

    func selection(text: String, prefix: String?, suffix: String?) -> NSRange? {
        if let mapped = mapped.sourceRange(text: text, prefix: prefix, suffix: suffix) { return mapped }
        guard let local = DesktopAXMappedText.uniqueRange(in: attributed.string, text: text, prefix: prefix, suffix: suffix),
              local.location <= range.length, local.length <= range.length - local.location else { return nil }
        return NSRange(location: range.location + local.location, length: local.length)
    }
}


struct DesktopAXInlineElement {
    let url: URL?
    let role: String?
    let description: String?
    let roleDescription: String?
}

enum DesktopAXTextStyle: Equatable {
    case heading(Int), quote(Int), list(level: Int, index: Int, prefix: String)
    case code, collapsed, bold, italic, underline, strike, superscript, subscriptText

    var block: Bool {
        switch self { case .heading, .quote, .list, .code, .collapsed: return true; default: return false }
    }

    func opening() throws -> String {
        switch self {
        case .heading(let level), .quote(let level):
            guard level >= 0, level < DesktopAXMappedText.maximumBytes else { throw Self.limit() }
            return String(repeating: self == .heading(level) ? "#" : ">", count: level) + " "
        case .list(let level, _, let prefix):
            guard level >= 0, level <= (DesktopAXMappedText.maximumBytes - prefix.utf8.count - 1) / 4 else { throw Self.limit() }
            return String(repeating: " ", count: level * 4) + prefix + " "
        case .code: return "```\n"
        case .collapsed: return "<details><summary>\n"
        case .bold: return "**"
        case .italic: return "*"
        case .underline: return "<u>"
        case .strike: return "~~"
        case .superscript: return "<sup>"
        case .subscriptText: return "<sub>"
        }
    }

    var closing: String {
        switch self {
        case .heading, .quote, .list: return ""
        case .code: return "\n```"
        case .collapsed: return "\n</summary>(collapsed content is hidden)</details>"
        case .bold: return "**"
        case .italic: return "*"
        case .underline: return "</u>"
        case .strike: return "~~"
        case .superscript: return "</sup>"
        case .subscriptText: return "</sub>"
        }
    }

    static func delimiters(closing: [Self], opening: [Self]) throws -> (String, String) {
        var close = "", open = ""
        for style in closing { close += style.closing }
        for style in opening {
            let value = try style.opening()
            guard value.utf8.count <= DesktopAXMappedText.maximumBytes - open.utf8.count else { throw limit() }
            if style.block { open = value + open } else { open += value }
        }
        return (close, open)
    }

    private static func limit() -> DesktopFailure {
        DesktopFailure(code: "ax_presentation_limit", message: "Native attributed formatting exceeds the bounded text domain")
    }
}

struct DesktopAXRunFormatter {
    private(set) var output: DesktopAXMappedText
    private var active: [DesktopAXTextStyle] = []

    init() throws { output = try DesktopAXMappedText() }

    mutating func append(_ input: DesktopAXMappedText, styles: [DesktopAXTextStyle]) throws {
        let old = active
        let removed = old.filter { !styles.contains($0) }
        let added = styles.filter { !old.contains($0) }
        active.removeAll { removed.contains($0) }
        active.append(contentsOf: added)
        var run = input
        if output.text.last == "\n" {
            output = try output.slice(NSRange(location: 0, length: output.offsets.count - 1))
            var moved = try DesktopAXMappedText("\n")
            try moved.append(run); run = moved
        }
        let leading = run.text.first == "\n" && run.text.count > 1
        if leading { run = try run.slice(NSRange(location: 1, length: run.offsets.count - 1)) }
        let block = styles.contains(where: \.block)
        if block {
            let trailing = run.text.last == "\n"
            if trailing { run = try run.slice(NSRange(location: 0, length: run.offsets.count - 1)) }
            let all = try DesktopAXTextStyle.delimiters(closing: styles, opening: styles)
            run = try run.replacingNewlines(with: DesktopAXMappedText(all.0 + "\n" + all.1))
            if trailing { try run.syntax("\n") }
            if leading {
                let transition = try DesktopAXTextStyle.delimiters(closing: old, opening: styles)
                var prefix = try DesktopAXMappedText(transition.0 + "\n" + transition.1)
                try prefix.append(run); try output.append(prefix)
                return
            }
        }
        let transition = try DesktopAXTextStyle.delimiters(closing: removed, opening: added)
        try output.syntax(transition.0 + (leading ? "\n" : "") + transition.1)
        try output.append(run)
    }

    mutating func finish() throws -> DesktopAXMappedText {
        try append(DesktopAXMappedText(), styles: [])
        return output
    }
}

struct DesktopAXAttributedFormatter {
    typealias InlineReader = (Any) throws -> DesktopAXInlineElement?

    static func render(_ attributed: NSAttributedString, sourceRange: NSRange, urls: inout DesktopAXURLRendering,
                       readInline: InlineReader, check: () throws -> Void) throws -> DesktopAXMappedText {
        guard sourceRange.location >= 0, sourceRange.location != NSNotFound, sourceRange.length == attributed.length,
              attributed.length <= 65536, sourceRange.location <= Int.max - attributed.length else {
            throw DesktopFailure(code: "invalid_ax_attributed_range", message: "Native attributed string does not match its retained bounded UTF-16 source range")
        }
        var formatter = try DesktopAXRunFormatter()
        var runs: [(NSRange, [NSAttributedString.Key: Any])] = []
        attributed.enumerateAttributes(in: NSRange(location: 0, length: attributed.length), options: []) { attributes, range, _ in
            runs.append((range, attributes))
        }
        for (range, attributes) in runs {
            try check()
            let source = attributed.attributedSubstring(from: range).string
            guard source.utf16.count == range.length else {
                throw DesktopFailure(code: "invalid_ax_attributed_range", message: "Native attributed run length differs from its UTF-16 string")
            }
            var run = try DesktopAXMappedText(source, sourceStart: sourceRange.location + range.location)
            let styles = try styles(attributes)
            if let link = attributes[.init(rawValue: kAXLinkTextAttribute.takeUnretainedValue() as String)], let element = try readInline(link), let url = element.url,
               let rendered = urls.render(url, isImage: element.role == "AXImage"), !rendered.isEmpty {
                let label = try run.trimmed()
                run = try DesktopAXMappedText("["); try run.append(label); try run.syntax("](" + rendered + ")")
            }
            if let attachment = attributes[.init(rawValue: kAXAttachmentTextAttribute.takeUnretainedValue() as String)], let element = try readInline(attachment) {
                let hasNewline = source.unicodeScalars.contains { CharacterSet.newlines.contains($0) }
                let hasLabel = !source.isEmpty && source != "\u{fffc}" && !hasNewline
                let label = hasLabel ? source : element.description
                let role = element.roleDescription ?? "attachment"
                guard role.utf16.count <= 65536, (label?.utf16.count ?? 0) <= 65536 else {
                    throw DesktopFailure(code: "ax_text_limit", message: "Native attachment label exceeds its text bound")
                }
                var rendered = try DesktopAXMappedText("[" + role + (label.map { $0.isEmpty ? "" : ": " + $0 } ?? "") + "]")
                if hasNewline { try rendered.syntax(" "); try rendered.append(run) }
                run = rendered
            }
            try formatter.append(run, styles: styles)
        }
        try check()
        return try formatter.finish()
    }

    static func styles(_ attributes: [NSAttributedString.Key: Any]) throws -> [DesktopAXTextStyle] {
        var result: [DesktopAXTextStyle] = []
        func add(_ style: DesktopAXTextStyle) { if !result.contains(style) { result.append(style) } }
        func number(_ key: String) -> NSNumber? { attributes[.init(rawValue: key)] as? NSNumber }
        if let values = attributes[.init(rawValue: kAXFontTextAttribute.takeUnretainedValue() as String)] as? [String: Any] {
            if let name = values[kAXFontNameKey.takeUnretainedValue() as String] as? String,
               let size = values[kAXFontSizeKey.takeUnretainedValue() as String] as? NSNumber, size.doubleValue.isFinite {
                var descriptor: [NSFontDescriptor.AttributeName: Any] = [.name: name]
                if let family = values[kAXFontFamilyKey.takeUnretainedValue() as String] as? String { descriptor[.family] = family }
                if let font = NSFont(descriptor: NSFontDescriptor(fontAttributes: descriptor), size: size.doubleValue) {
                    let traits = CTFontGetSymbolicTraits(font as CTFont)
                    if traits.contains(.traitBold) { add(.bold) }
                    if traits.contains(.traitItalic) { add(.italic) }
                }
            }
            if values["AXFontBold"] as? Bool == true { add(.bold) }
        }
        if let value = number(kAXUnderlineTextAttribute.takeUnretainedValue() as String), value.intValue >= 1 { add(.underline) }
        if number(kAXStrikethroughTextAttribute.takeUnretainedValue() as String)?.boolValue == true { add(.strike) }
        if let value = number(kAXSuperscriptTextAttribute.takeUnretainedValue() as String)?.intValue {
            if value > 0 { add(.superscript) } else if value < 0 { add(.subscriptText) }
        }
        if let level = number("AXBlockQuoteLevel")?.intValue, level > 0 { add(.quote(level)) }
        if let names = attributes[.init(rawValue: "AXStyleName")] as? String {
            guard names.utf16.count <= 65536 else { throw DesktopFailure(code: "ax_text_limit", message: "Native style names exceed the attributed text bound") }
            for name in names.components(separatedBy: ", ") {
                switch name {
                case "Title": add(.heading(1))
                case "Heading": add(.heading(2))
                case "Subheading": add(.heading(3))
                case "Fixed width": add(.code)
                case "Collapsed": add(.collapsed)
                default: break
                }
            }
        }
        if let level = number(kAXListItemLevelTextAttribute.takeUnretainedValue() as String), let index = number(kAXListItemIndexTextAttribute.takeUnretainedValue() as String),
           let prefix = attributes[.init(rawValue: kAXListItemPrefixTextAttribute.takeUnretainedValue() as String)] as? NSAttributedString {
            guard prefix.length <= 65536 else { throw DesktopFailure(code: "ax_text_limit", message: "Native list prefix exceeds its text bound") }
            let text = prefix.string
            let rendered: String
            if text == "checklist item, incomplete" { rendered = "* [ ]" }
            else if text == "checklist item, completed" { rendered = "* [x]" }
            else if text.first?.isNumber == true { rendered = index.stringValue + "." }
            else { rendered = "*" }
            add(.list(level: level.intValue, index: index.intValue, prefix: rendered))
        }
        return result
    }
}

struct DesktopAXAttributedInput {
    let attributed: NSAttributedString
    let range: NSRange
    let inline: [(AXUIElement, DesktopAXInlineElement)]

    func render(urls: inout DesktopAXURLRendering, check: () throws -> Void) throws -> DesktopAXAttributedSource {
        let mapped = try DesktopAXAttributedFormatter.render(attributed, sourceRange: range, urls: &urls, readInline: { value in
            guard CFGetTypeID(value as CFTypeRef) == AXUIElementGetTypeID() else { return nil }
            return inline.first { CFEqual($0.0, value as CFTypeRef) }?.1
        }, check: check)
        return DesktopAXAttributedSource(attributed: attributed, range: range, source: "ax_attributed_range", mapped: mapped)
    }
}
