// Draws the OnEum app icon and writes assets/icon-1024.png.
// Usage: swift scripts/gen-icon.swift assets/icon-1024.png
import AppKit

let size: CGFloat = 1024
let out = CommandLine.arguments.count > 1 ? CommandLine.arguments[1] : "icon-1024.png"
let space = CGColorSpaceCreateDeviceRGB()
guard let ctx = CGContext(data: nil, width: Int(size), height: Int(size), bitsPerComponent: 8, bytesPerRow: 0,
                          space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { exit(1) }

// macOS icon grid: the artwork sits inside a 824pt rounded square with margins.
let inset: CGFloat = 100
let rect = CGRect(x: inset, y: inset, width: size - inset * 2, height: size - inset * 2)
let shape = CGPath(roundedRect: rect, cornerWidth: 185, cornerHeight: 185, transform: nil)

ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: CGColor(gray: 0, alpha: 0.30))
ctx.addPath(shape)
ctx.setFillColor(CGColor(red: 0.33, green: 0.31, blue: 0.84, alpha: 1))
ctx.fillPath()
ctx.restoreGState()

ctx.saveGState()
ctx.addPath(shape)
ctx.clip()
let colors = [CGColor(red: 0.55, green: 0.52, blue: 0.98, alpha: 1), CGColor(red: 0.26, green: 0.22, blue: 0.70, alpha: 1)] as CFArray
let gradient = CGGradient(colorsSpace: space, colors: colors, locations: [0, 1])!
ctx.drawLinearGradient(gradient, start: CGPoint(x: rect.minX, y: rect.maxY), end: CGPoint(x: rect.maxX, y: rect.minY), options: [])

// Three concentric arcs (sound) around a solid dot: "on" + "eum" (sound).
let centre = CGPoint(x: size / 2, y: size / 2 - 10)
ctx.setStrokeColor(CGColor(gray: 1, alpha: 0.95))
ctx.setLineCap(.round)
for (radius, width, alpha) in [(150.0, 46.0, 1.0), (245.0, 40.0, 0.78), (335.0, 34.0, 0.5)] {
    ctx.setLineWidth(CGFloat(width))
    ctx.setStrokeColor(CGColor(gray: 1, alpha: CGFloat(alpha)))
    ctx.addArc(center: centre, radius: CGFloat(radius), startAngle: .pi * 0.22, endAngle: .pi * 0.78, clockwise: false)
    ctx.strokePath()
    ctx.addArc(center: centre, radius: CGFloat(radius), startAngle: .pi * 1.22, endAngle: .pi * 1.78, clockwise: false)
    ctx.strokePath()
}
ctx.setFillColor(CGColor(gray: 1, alpha: 1))
ctx.fillEllipse(in: CGRect(x: centre.x - 62, y: centre.y - 62, width: 124, height: 124))
ctx.restoreGState()

guard let image = ctx.makeImage() else { exit(1) }
let rep = NSBitmapImageRep(cgImage: image)
guard let data = rep.representation(using: .png, properties: [:]) else { exit(1) }
try data.write(to: URL(fileURLWithPath: out))
print("wrote \(out)")
