import AppKit
import Foundation
import Markdown

private final class DesktopPasteResourceBlocker: NSObject {
    @objc(webView:resource:willSendRequest:redirectResponse:fromDataSource:)
    func webView(_ view: AnyObject, resource: AnyObject, willSendRequest request: NSURLRequest,
                 redirectResponse: URLResponse?, fromDataSource: AnyObject) -> NSURLRequest? {
        nil
    }
}

@MainActor
enum DesktopPasteFormat {
    enum Kind: String, Codable {
        case text
        case markdown = "md"
        case html
    }

    static func convert(_ text: String, format: Kind,
                        check: () throws -> Void) throws -> DesktopPasteRepresentations {
        try Task.checkCancellation()
        try check()
        guard text.utf8.count <= 1_048_576 else {
            throw DesktopFailure(code: "invalid_paste_representation", message: "Paste source exceeds the native request bound")
        }
        if format == .text { return try DesktopPasteRepresentations.text(text) }
        let html: String
        switch format {
        case .markdown:
            html = HTMLFormatter.format(text, options: [])
        case .html:
            html = text
        case .text:
            throw DesktopFailure(code: "invalid_paste_representation", message: "Rich conversion requires Markdown or HTML")
        }
        try Task.checkCancellation()
        try check()
        let htmlData = Data(html.utf8)
        guard htmlData.count <= 1_048_576 else {
            throw DesktopFailure(code: "invalid_paste_representation", message: "Converted HTML exceeds the native request bound")
        }
        let blocker = DesktopPasteResourceBlocker()
        let attributed: NSAttributedString
        do {
            attributed = try withExtendedLifetime(blocker) {
                try NSAttributedString(data: htmlData, options: [
                    .documentType: NSAttributedString.DocumentType.html,
                    .characterEncoding: String.Encoding.utf8.rawValue,
                    .timeout: 2.0,
                    .webResourceLoadDelegate: blocker
                ], documentAttributes: nil)
            }
        } catch {
            throw DesktopFailure(code: "paste_conversion_failed", message: "Native HTML conversion failed: \(error)")
        }
        try Task.checkCancellation()
        try check()
        let nativeText = NSMutableAttributedString(attributedString: attributed)
        let range = NSRange(location: 0, length: nativeText.length)
        nativeText.removeAttribute(NSAttributedString.Key("viewAttachment"), range: range)
        guard let rtf = nativeText.rtf(from: range, documentAttributes: [:]) else {
            throw DesktopFailure(code: "paste_conversion_failed", message: "Native attributed text could not produce RTF")
        }
        try Task.checkCancellation()
        try check()
        return try DesktopPasteRepresentations.formatted(html: htmlData, rtf: rtf, plainText: attributed.string)
    }
}
