import { defineShikiSetup } from '@slidev/types'

// Code on the slides is what a terminal or a config file shows: dark, like a
// terminal, even though the deck itself is light.
export default defineShikiSetup(() => ({
  themes: {
    dark: 'github-dark',
    light: 'github-dark',
  },
}))
