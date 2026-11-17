// The one piece of Swift in the app itself. WidgetKit, which tells the system that
// what the widgets show is out of date, can be reached from Swift only; the app is
// not written in Swift, so this function is all there is (see build.rs, which
// compiles it, and src/platform/ios/widget.rs, which calls it).
import WidgetKit

@_cdecl("oneum_reload_widgets")
public func oneumReloadWidgets() {
    WidgetCenter.shared.reloadAllTimelines()
}
