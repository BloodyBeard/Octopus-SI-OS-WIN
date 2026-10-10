import AppKit
import Foundation

let fileManager = FileManager.default
let appRoot = URL(fileURLWithPath: fileManager.currentDirectoryPath)
let publicDir = appRoot.appendingPathComponent("public", isDirectory: true)
let glyphURL = publicDir.appendingPathComponent("octopus-glyph.png")
let legacyLogoURL = publicDir.appendingPathComponent("octopus-logo.png")
let appIconsetURL = publicDir.appendingPathComponent("octopus.iconset", isDirectory: true)

if !fileManager.fileExists(atPath: glyphURL.path) {
  try fileManager.copyItem(at: legacyLogoURL, to: glyphURL)
}

guard let glyph = NSImage(contentsOf: glyphURL) else {
  fatalError("Cannot read glyph source at \(glyphURL.path)")
}

try fileManager.createDirectory(
  at: appIconsetURL,
  withIntermediateDirectories: true,
)

try replaceFile(from: glyphURL, to: legacyLogoURL)
try writePNG(renderTransparentIcon(size: 96, glyph: glyph, glyphScale: 0.94), to: publicDir.appendingPathComponent("favicon.png"))
try writePNG(renderDevelopmentDockIcon(size: 1024, glyph: glyph), to: publicDir.appendingPathComponent("octopus-dev-dock-icon.png"))

let appIconOutputs: [(String, Int)] = [
  ("octopus-app-icon.png", 1024),
  ("octopus.iconset/icon_16x16.png", 16),
  ("octopus.iconset/icon_16x16@2x.png", 32),
  ("octopus.iconset/icon_32x32.png", 32),
  ("octopus.iconset/icon_32x32@2x.png", 64),
  ("octopus.iconset/icon_128x128.png", 128),
  ("octopus.iconset/icon_128x128@2x.png", 256),
  ("octopus.iconset/icon_256x256.png", 256),
  ("octopus.iconset/icon_256x256@2x.png", 512),
  ("octopus.iconset/icon_512x512.png", 512),
  ("octopus.iconset/icon_512x512@2x.png", 1024),
]

for (relativePath, size) in appIconOutputs {
  let image = renderTransparentIcon(size: size, glyph: glyph, glyphScale: 1.0 / 1.5)
  let destination = publicDir.appendingPathComponent(relativePath)
  try fileManager.createDirectory(
    at: destination.deletingLastPathComponent(),
    withIntermediateDirectories: true,
  )
  try writePNG(image, to: destination)
}

func renderTransparentIcon(size: Int, glyph: NSImage, glyphScale: CGFloat) -> NSImage {
  let dimension = CGFloat(size)
  let image = NSImage(size: NSSize(width: dimension, height: dimension))
  image.lockFocus()
  NSGraphicsContext.current?.imageInterpolation = .high
  NSColor.clear.setFill()
  NSRect(x: 0, y: 0, width: dimension, height: dimension).fill()

  let glyphSize = dimension * glyphScale
  let glyphRect = NSRect(
    x: (dimension - glyphSize) / 2,
    y: (dimension - glyphSize) / 2,
    width: glyphSize,
    height: glyphSize
  )
  glyph.draw(in: glyphRect, from: .zero, operation: .sourceOver, fraction: 1)

  image.unlockFocus()
  return image
}

func renderDevelopmentDockIcon(size: Int, glyph: NSImage) -> NSImage {
  let dimension = CGFloat(size)
  let image = NSImage(size: NSSize(width: dimension, height: dimension))
  image.lockFocus()
  NSGraphicsContext.current?.imageInterpolation = .high
  NSColor.clear.setFill()
  NSRect(x: 0, y: 0, width: dimension, height: dimension).fill()

  let inset = dimension * 0.055
  let iconRect = NSRect(x: inset, y: inset, width: dimension - inset * 2, height: dimension - inset * 2)
  let iconPath = NSBezierPath(roundedRect: iconRect, xRadius: dimension * 0.225, yRadius: dimension * 0.225)
  let shadow = NSShadow()
  shadow.shadowColor = NSColor.black.withAlphaComponent(0.42)
  shadow.shadowBlurRadius = dimension * 0.036
  shadow.shadowOffset = NSSize(width: 0, height: -dimension * 0.012)
  shadow.set()
  NSColor(calibratedWhite: 0.13, alpha: 1).setFill()
  iconPath.fill()

  NSGraphicsContext.saveGraphicsState()
  iconPath.addClip()
  NSGradient(
    starting: NSColor(calibratedWhite: 0.22, alpha: 1),
    ending: NSColor(calibratedWhite: 0.105, alpha: 1)
  )?.draw(in: iconRect, angle: -90)
  NSGraphicsContext.restoreGraphicsState()

  NSColor.white.withAlphaComponent(0.14).setStroke()
  iconPath.lineWidth = max(1, dimension * 0.002)
  iconPath.stroke()

  let glyphSize = dimension * 0.57
  let glyphRect = NSRect(
    x: (dimension - glyphSize) / 2,
    y: (dimension - glyphSize) / 2 - dimension * 0.006,
    width: glyphSize,
    height: glyphSize
  )
  glyph.draw(in: glyphRect, from: .zero, operation: .sourceOver, fraction: 0.9)

  image.unlockFocus()
  return image
}

func replaceFile(from source: URL, to destination: URL) throws {
  if fileManager.fileExists(atPath: destination.path) {
    try fileManager.removeItem(at: destination)
  }
  try fileManager.copyItem(at: source, to: destination)
}

func writePNG(_ image: NSImage, to destination: URL) throws {
  guard
    let tiff = image.tiffRepresentation,
    let bitmap = NSBitmapImageRep(data: tiff),
    let data = bitmap.representation(using: .png, properties: [:])
  else {
    fatalError("Cannot encode PNG for \(destination.path)")
  }
  try data.write(to: destination)
}
