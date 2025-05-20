pub(crate) enum AppEvent {
    // Scroll event with a delta where positive is down and negative is up
    // This can be used for Go-To-Bottom and Go-To-Top events where the delta is
    // isize::MIN and isize::MAX respectively
    Scroll(isize),

    // Bring up a different/expanded view of an item
    Expand,

    // Collapse the expanded view of an item
    Collapse,

    // Open the item in the default application (e.g. browser)
    Open,
}
