pub(crate) enum AppEvent {
    // Scroll event with a delta where positive is down and negative is up
    // This can be used for Go-To-Bottom and Go-To-Top events where the delta is
    // isize::MIN and isize::MAX respectively
    Scroll(isize),

    // Enter a new view (e.g. a new screen or popup)
    Expand,

    // Close a expanded/nested view (e.g. a popup or screen that is triggered by a parent widget)
    Close,

    // Open the item in the default (external) application (e.g. browser)
    Open,

    // Exit the application - akin to a kill switch
    Exit,
}
