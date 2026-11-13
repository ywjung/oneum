// Draws the OnEum app icon and writes assets/icon-1024.png.
// Usage: swift scripts/gen-icon.swift assets/icon-1024.png
//
// The mark is the name: 온음 is "the whole sound", and a 온음표 is a whole note. So the
// icon is a whole note (which is also the O of OnEum) lying on the lines of a staff.
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

// The plate: the app's violet, lighter at the top left.
ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -12), blur: 28, color: CGColor(gray: 0, alpha: 0.30))
ctx.addPath(shape)
ctx.setFillColor(CGColor(red: 0.33, green: 0.31, blue: 0.84, alpha: 1))
ctx.fillPath()
ctx.restoreGState()

ctx.saveGState()
ctx.addPath(shape)
ctx.clip()
let colors = [CGColor(red: 0.58, green: 0.55, blue: 0.99, alpha: 1), CGColor(red: 0.24, green: 0.20, blue: 0.68, alpha: 1)] as CFArray
let gradient = CGGradient(colorsSpace: space, colors: colors, locations: [0, 1])!
ctx.drawLinearGradient(gradient, start: CGPoint(x: rect.minX, y: rect.maxY), end: CGPoint(x: rect.maxX, y: rect.minY), options: [])

// A staff: five lines across the plate, quiet enough to stay in the background.
let centre = CGPoint(x: size / 2, y: size / 2)
let lineGap: CGFloat = 118
ctx.setLineWidth(15)
ctx.setStrokeColor(CGColor(gray: 1, alpha: 0.26))
for step in -2...2 {
    let y = centre.y + CGFloat(step) * lineGap
    ctx.move(to: CGPoint(x: rect.minX, y: y))
    ctx.addLine(to: CGPoint(x: rect.maxX, y: y))
    ctx.strokePath()
}

// The whole note, on the middle line: a broad oval with a slanted hole, which is what
// tells a note head from a plain ring. Filled even-odd, so the staff shows through.
let noteWidth: CGFloat = 520
let noteHeight: CGFloat = 352
let note = CGMutablePath()
note.addEllipse(in: CGRect(x: centre.x - noteWidth / 2, y: centre.y - noteHeight / 2, width: noteWidth, height: noteHeight))
let holeWidth: CGFloat = 318
let holeHeight: CGFloat = 176
var slant = CGAffineTransform(translationX: centre.x, y: centre.y).rotated(by: -.pi * 0.20)
note.addEllipse(in: CGRect(x: -holeWidth / 2, y: -holeHeight / 2, width: holeWidth, height: holeHeight), transform: slant)

ctx.saveGState()
ctx.setShadow(offset: CGSize(width: 0, height: -10), blur: 24, color: CGColor(red: 0.10, green: 0.08, blue: 0.35, alpha: 0.45))
ctx.addPath(note)
ctx.setFillColor(CGColor(gray: 1, alpha: 1))
ctx.fillPath(using: .evenOdd)
ctx.restoreGState()
ctx.restoreGState()

guard let image = ctx.makeImage() else { exit(1) }
let rep = NSBitmapImageRep(cgImage: image)
guard let data = rep.representation(using: .png, properties: [:]) else { exit(1) }
try data.write(to: URL(fileURLWithPath: out))
print("wrote \(out)")
