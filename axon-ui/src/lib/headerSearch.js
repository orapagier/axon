import { computed, onMounted, onUnmounted, reactive, unref } from 'vue'

// One search field lives in the shell topbar (App.vue). Each page that has
// searchable content registers a scope while mounted, and the field binds to
// the active page's scope — so its placeholder and target follow navigation.
// Pages keep owning their query refs: filtering and server-search logic is
// unchanged, only the input moved out of the page.
//
// Scope shape:
//   query        Ref<string> | WritableComputedRef<string> — the page's query
//   placeholder  string | Ref<string>
//   visible      (optional) Ref<boolean> — hide the field while false
//   onSubmit     (optional) called when the user presses Enter (for
//                server-driven searches; live filters just watch `query`)
const scopes = reactive({})

export function useHeaderSearch(pageId, scope) {
  onMounted(() => {
    scopes[pageId] = scope
  })
  onUnmounted(() => {
    if (scopes[pageId] === scope) delete scopes[pageId]
  })
}

// Reactive lookup for App.vue: the active page's scope, or null when the page
// has nothing to search (the field hides).
export function headerSearchFor(pageId) {
  return computed(() => {
    const s = scopes[unref(pageId)]
    if (!s || ('visible' in s && !unref(s.visible))) return null
    return s
  })
}
