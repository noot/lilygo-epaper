// a page's remotely produced view — fetched over wifi or loaded from the sd
// card — plus the flag that a fresh load is wanted on the next pass. every
// data-backed page runs the same two-step machine: mark the state dirty (so
// the loading view can paint first), then kick the load off exactly once and
// store the result when it lands.
pub(crate) struct Remote<T> {
    pub(crate) view: T,
    dirty: bool,
}

impl<T> Remote<T> {
    // a page's boot state. `active` is whether the page's screen is already
    // showing (a deep-sleep wake restores the last screen): it must load on
    // the first pass then, or the page wakes stuck on its placeholder view
    // with nothing scheduled to replace it.
    pub(crate) fn new(view: T, active: bool) -> Self {
        Self {
            view,
            dirty: active,
        }
    }

    // request a (re)load on the next pass, showing `loading` until it lands.
    pub(crate) fn refresh(&mut self, loading: T) {
        self.view = loading;
        self.dirty = true;
    }

    // request a (re)load on the next pass, keeping the current view up.
    pub(crate) fn invalidate(&mut self) {
        self.dirty = true;
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.dirty
    }

    // the requested load has been started (or served): stop asking for it.
    pub(crate) fn clear(&mut self) {
        self.dirty = false;
    }
}
