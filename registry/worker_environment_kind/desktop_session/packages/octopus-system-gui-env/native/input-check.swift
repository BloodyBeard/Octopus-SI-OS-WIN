import ApplicationServices
import Carbon.HIToolbox
import CoreGraphics
import Foundation

@main struct DesktopInputCheck {
    static func main() async throws {
        let (code, flags) = try DesktopInput.key("cmd+shift+p")
        precondition(code == CGKeyCode(kVK_ANSI_P) && flags.contains(.maskCommand) && flags.contains(.maskShift))
        precondition(tryKey("9") == CGKeyCode(kVK_ANSI_9))
        precondition(tryKey("f20") == CGKeyCode(kVK_F20))
        precondition(tryKey("keypadenter") == CGKeyCode(kVK_ANSI_KeypadEnter))
        precondition(tryKey("jiskana") == CGKeyCode(kVK_JIS_Kana))
        for bad in ["cmd++a", "cmd+cmd+a", "hyper+a", "f21", " ", "😀"] {
            do { _ = try DesktopInput.key(bad); preconditionFailure("Unsupported chord was accepted") } catch is DesktopFailure { }
        }
        let (button, down, up, count) = try DesktopInput.click(["button":"right", "count":2])
        precondition(button == .right && down == .rightMouseDown && up == .rightMouseUp && count == 2)
        let middle = try DesktopInput.click(["button":"middle"])
        precondition(middle.0 == .center && middle.1 == .otherMouseDown && middle.3 == 1)
        for bad: [String:Any] in [["count":true], ["count":1.5], ["count":0], ["count":3], ["button":"unknown"], ["button":NSNull()]] {
            do { _ = try DesktopInput.click(bad); preconditionFailure("Invalid click was accepted") } catch is DesktopFailure { }
        }
        print("Native pure input contract passed: SDK key constants/chords and mouse button/count. No native events constructed or posted.")
        let attributed = NSMutableAttributedString(string: "A😀e\u{301} Z")
        attributed.addAttribute(NSAttributedString.Key(kAXUnderlineTextAttribute.takeUnretainedValue() as String), value: NSNumber(1), range: NSRange(location: 1, length: 4))
        var urls = DesktopAXURLRendering()
        let mapped = try DesktopAXAttributedFormatter.render(attributed, sourceRange: NSRange(location: 10, length: attributed.length),
            urls: &urls, readInline: { _ in nil }, check: {})
        precondition(mapped.text == "A<u>😀e\u{301}</u> Z")
        precondition(mapped.sourceRange(text: "😀e\u{301}", prefix: "<u>", suffix: "</u>") == NSRange(location: 11, length: 4))
        precondition(mapped.sourceRange(for: NSRange(location: 1, length: 3)) == nil)
        precondition(DesktopAXMappedText.uniqueRange(in: "aaa", text: "aa", prefix: nil, suffix: nil) == nil)
        let profile = DesktopAXPresentationProfile(context_type: "full_ui", hide_cancel_actions: false,
            omit_calendar_event_children: false, excluded_action_descriptions: [], capture_note: nil)
        let child = DesktopAXRenderedNode(id: "child", text: "text original", children: [])
        let initial = DesktopAXRenderedTree(roots: [.init(id: "root", text: "container", children: [child])], facts: [],
            windowTitle: "Window", applicationName: "App", selectedText: nil, profile: profile)
        let changed = DesktopAXRenderedTree(roots: [.init(id: "root", text: "container", children: [.init(id: "child", text: "text changed", children: [])])],
            facts: [], windowTitle: "Window", applicationName: "App", selectedText: nil, profile: profile)
        let full = try initial.presentation(base: nil, baseRevision: nil)
        precondition(full.mode == "full" && full.base_revision == nil && full.rendered_element_ids == ["root", "child"])
        let diff = try changed.presentation(base: initial, baseRevision: 1)
        precondition(diff.mode == "diff" && diff.base_revision == 1 && diff.text.hasSuffix("~\tchild text changed"))
        let unchanged = try changed.presentation(base: changed, baseRevision: 2)
        precondition(unchanged.mode == "no_change" && unchanged.base_revision == 2)
        let prior = DesktopAXRenderedTree(roots: [.init(id: "root", text: "container", children: [child,
            .init(id: "removed-a", text: "old a", children: []), .init(id: "removed-b", text: "old b", children: [])])],
            facts: [], windowTitle: "Window", applicationName: "App", selectedText: nil, profile: profile)
        let equalCost = try initial.presentation(base: prior, baseRevision: 1)
        precondition(equalCost.mode == "diff" && equalCost.text.contains("-\tremoved-a old a") && equalCost.text.contains("-\tremoved-b old b"))
        let pipe = Pipe()
        let parent = DesktopParentChannel(pipe.fileHandleForReading)
        let packets = parent.packets()
        try pipe.fileHandleForWriting.write(contentsOf: Data("{\"id\":1}\n".utf8))
        try pipe.fileHandleForWriting.close()
        var packetCount = 0
        for try await packet in packets { precondition(String(data:packet,encoding:.utf8) == "{\"id\":1}"); packetCount += 1 }
        precondition(packetCount == 1 && !parent.isOpen)
        print("Native pure parent channel: identified packet followed by EOF closes action precondition.")
    }
    static func tryKey(_ key: String) -> CGKeyCode { try! DesktopInput.key(key).0 }
}
