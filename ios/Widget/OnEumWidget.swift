// The home and lock screen widgets: the song that plays, or the one that was on last,
// and whether it plays.
//
// A widget is drawn by this extension, a process of its own that the system starts
// when it wants a widget drawn. It cannot ask the app anything: the app leaves the
// song (title, artist, a small copy of the cover) in a group of keychain items that
// both may read, and tells the system when that changed
// (src/platform/ios/widget.rs, which says why the keychain). A tap on a widget opens
// the app.
import ImageIO
import Security
import SwiftUI
import WidgetKit

/// What the app left for the widgets: items of the keychain group both share.
enum Left {
    /// The group's full name (with the team's prefix), which the build writes into
    /// this extension's Info.plist as it does into the app's.
    static let group = Bundle.main.object(forInfoDictionaryKey: "OneumSharedKeychain") as? String

    /// The item kept under `account` ("song", "cover"), if there is one.
    static func read(_ account: String) -> Data? {
        guard let group = group, !group.isEmpty else { return nil }
        let query: [CFString: Any] = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: "OnEum widgets",
            kSecAttrAccount: account,
            kSecAttrAccessGroup: group,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne,
        ]
        var found: CFTypeRef?
        guard SecItemCopyMatching(query as CFDictionary, &found) == errSecSuccess else {
            return nil
        }
        return found as? Data
    }
}

/// Whether the song plays, as far as the widgets can tell.
enum Playing {
    case playing
    case paused
    /// Not told (an earlier version of the app), or it played until a time that has
    /// passed: the app may have been ended without a word.
    case unknown
}

/// What the app left: the song, its cover if it has one, and whether it plays.
struct Song {
    let title: String
    let artist: String
    let cover: UIImage?
    /// The app says it plays until then (where it says nothing more); `nil` with
    /// `paused`: it said the song is paused.
    let playingUntil: Date?
    let paused: Bool

    static func load() -> Song? {
        guard let data = Left.read("song"),
            let told = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
            let title = told["title"] as? String, !title.isEmpty
        else { return nil }
        let cover = (told["cover"] as? Bool) == true ? Left.read("cover").flatMap(small) : nil
        let until = (told["playing_until"] as? NSNumber).map {
            Date(timeIntervalSince1970: $0.doubleValue)
        }
        // (A key that is there with nothing in it says "paused"; none at all says
        // nothing.)
        let paused = until == nil && told.keys.contains("playing_until")
        return Song(
            title: title, artist: told["artist"] as? String ?? "", cover: cover,
            playingUntil: until, paused: paused)
    }

    /// Whether it plays at `date`.
    func playing(at date: Date) -> Playing {
        if let until = playingUntil {
            return date < until ? .playing : .unknown
        }
        return paused ? .paused : .unknown
    }

    /// The picture in `data`, no larger than a widget shows it. The system does not
    /// draw a widget whose pictures are too large, and an extension has little memory:
    /// the picture is never decoded at more than this size.
    static func small(_ data: Data) -> UIImage? {
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: 320,
        ]
        guard let source = CGImageSourceCreateWithData(data as CFData, nil),
            let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary)
        else { return nil }
        return UIImage(cgImage: image)
    }
}

struct SongEntry: TimelineEntry {
    let date: Date
    let song: Song?
    let playing: Playing
}

/// The song now, and where it is said to play, the same song no longer playing from
/// the time the app named on. Nothing more is due: the app says when anything changed.
struct SongProvider: TimelineProvider {
    func placeholder(in context: Context) -> SongEntry {
        SongEntry(date: Date(), song: nil, playing: .unknown)
    }

    func getSnapshot(in context: Context, completion: @escaping (SongEntry) -> Void) {
        let now = Date()
        let song = Song.load()
        completion(SongEntry(date: now, song: song, playing: song?.playing(at: now) ?? .unknown))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<SongEntry>) -> Void) {
        let now = Date()
        let song = Song.load()
        var entries = [SongEntry(date: now, song: song, playing: song?.playing(at: now) ?? .unknown)]
        if let until = song?.playingUntil, until > now {
            entries.append(SongEntry(date: until, song: song, playing: .unknown))
        }
        completion(Timeline(entries: entries, policy: .never))
    }
}

private let korean = Locale.preferredLanguages.first?.hasPrefix("ko") ?? false
private let nothing = korean ? "재생 중인 곡 없음" : "Nothing playing"
private let playingWord = korean ? "재생 중" : "Playing"
private let pausedWord = korean ? "일시정지" : "Paused"
/// The app's accent on its dark ground (src/ui/theme.rs).
private let accent = Color(red: 0.604, green: 0.592, blue: 0.961)

extension Playing {
    /// The symbol that says it, where there is something to say.
    var symbol: String? {
        switch self {
        case .playing: return "waveform"
        case .paused: return "pause.fill"
        case .unknown: return nil
        }
    }

    var word: String? {
        switch self {
        case .playing: return playingWord
        case .paused: return pausedWord
        case .unknown: return nil
        }
    }
}

/// Whether the song plays, in a symbol and a word (nothing where that is not known).
struct PlayingView: View {
    let playing: Playing
    /// With the word, or the symbol alone (where there is no room).
    var worded = true

    var body: some View {
        if let symbol = playing.symbol, let word = playing.word {
            HStack(spacing: 4) {
                Image(systemName: symbol).font(.caption2.weight(.bold))
                if worded {
                    Text(word).font(.caption2.weight(.semibold)).lineLimit(1)
                }
            }
            .foregroundColor(playing == .playing ? accent : .white.opacity(0.6))
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(word)
        }
    }
}

/// The cover, or a note where a song has none.
struct CoverView: View {
    let cover: UIImage?

    var body: some View {
        if let cover = cover {
            Image(uiImage: cover).resizable().aspectRatio(contentMode: .fill)
        } else {
            ZStack {
                Color.white.opacity(0.08)
                Image(systemName: "music.note").font(.title2).foregroundColor(.white.opacity(0.7))
            }
        }
    }
}

struct SongView: View {
    @Environment(\.widgetFamily) private var family
    let entry: SongEntry

    private var title: String { entry.song?.title ?? "OnEum" }
    private var artist: String { entry.song.map { $0.artist } ?? nothing }

    var body: some View {
        switch family {
        case .accessoryInline:
            // (One line of the system's: a symbol and the title.)
            Text("\(Image(systemName: entry.playing.symbol ?? "music.note")) \(title)")
                .accessibilityLabel([entry.playing.word, title].compactMap { $0 }.joined(separator: ", "))
        case .accessoryCircular:
            ZStack {
                AccessoryWidgetBackground()
                Image(systemName: entry.playing.symbol ?? "music.note").font(.title3)
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel([entry.playing.word, title].compactMap { $0 }.joined(separator: ", "))
        case .accessoryRectangular:
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 4) {
                    if let symbol = entry.playing.symbol {
                        Image(systemName: symbol).font(.caption.weight(.bold))
                            .accessibilityLabel(entry.playing.word ?? "")
                    }
                    Text(title).font(.headline).lineLimit(1)
                }
                Text(artist).font(.subheadline).lineLimit(1).opacity(0.8)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        case .systemMedium:
            HStack(spacing: 14) {
                CoverView(cover: entry.song?.cover)
                    .frame(width: 104, height: 104)
                    .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                VStack(alignment: .leading, spacing: 8) {
                    words(titleFont: .headline, lines: 2)
                    PlayingView(playing: entry.playing)
                }
                Spacer(minLength: 0)
            }
            .padding(16)
        default:
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .top) {
                    CoverView(cover: entry.song?.cover)
                        .frame(width: 64, height: 64)
                        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                    Spacer(minLength: 4)
                    // (Beside the cover: below the words there is no room for it.)
                    PlayingView(playing: entry.playing, worded: false)
                }
                words(titleFont: .subheadline.weight(.semibold), lines: 1)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            .padding(14)
        }
    }

    private func words(titleFont: Font, lines: Int) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(title).font(titleFont).foregroundColor(.white).lineLimit(lines)
            Text(artist).font(.caption).foregroundColor(.white.opacity(0.7)).lineLimit(1)
        }
    }
}

/// The app's dark behind the widgets of the home screen (those of the lock screen have
/// the system's own ground).
private let ground = Color(red: 0.106, green: 0.106, blue: 0.129)

@main
struct NowPlayingWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: "NowPlaying", provider: SongProvider()) { entry in
            if #available(iOSApplicationExtension 17.0, *) {
                SongView(entry: entry).containerBackground(ground, for: .widget)
            } else {
                ZStack {
                    ground
                    SongView(entry: entry)
                }
            }
        }
        .configurationDisplayName(korean ? "재생 중인 곡" : "Now Playing")
        .description(korean ? "재생 중인 곡과 그 상태, 또는 마지막으로 들은 곡." : "The song that is playing and whether it plays, or the one that was on last.")
        .supportedFamilies([
            .systemSmall, .systemMedium, .accessoryRectangular, .accessoryInline, .accessoryCircular,
        ])
        .contentMarginsDisabledIfPossible()
    }
}

extension WidgetConfiguration {
    /// From iOS 17 the system adds margins of its own around a widget's content; the
    /// views here bring theirs.
    func contentMarginsDisabledIfPossible() -> some WidgetConfiguration {
        if #available(iOSApplicationExtension 17.0, *) {
            return self.contentMarginsDisabled()
        }
        return self
    }
}
