import Foundation
import ImageIO
import Vision

let arguments = Array(CommandLine.arguments.dropFirst())
guard let path = arguments.first, !arguments.dropFirst().isEmpty else {
    fputs("usage: stage7_ocr.swift IMAGE MARKER...\n", stderr)
    exit(2)
}
let markers = arguments.dropFirst().map { $0.lowercased() }
guard let source = CGImageSourceCreateWithURL(URL(fileURLWithPath: path) as CFURL, nil),
      let image = CGImageSourceCreateImageAtIndex(source, 0, nil) else {
    fputs("could not read image\n", stderr)
    exit(1)
}

let request = VNRecognizeTextRequest()
request.recognitionLevel = .fast
request.usesLanguageCorrection = false
request.recognitionLanguages = ["en-US"]
let handler = VNImageRequestHandler(cgImage: image, options: [:])
do {
    try handler.perform([request])
} catch {
    fputs("OCR failed: \(error)\n", stderr)
    exit(1)
}
let text = (request.results ?? [])
    .compactMap { $0.topCandidates(1).first?.string }
    .joined(separator: "\n")
    .lowercased()
let missing = markers.filter { !text.contains($0) }
if !missing.isEmpty {
    fputs("missing screenshot markers: \(missing.joined(separator: ", "))\n", stderr)
    exit(1)
}
print(text)
