<script setup lang="ts">
// One line icon per tool, from its ballet name. Used in The cast, the stack
// and the titles of slides about a tool. Not-built tools are drawn grey.
const props = defineProps<{ name: 'jete' | 'jete-agent' | 'barre' | 'mime' | 'regisseur' }>()
const planned = ['mime', 'regisseur'].includes(props.name)
</script>

<template>
  <span class="tool-icon" :class="{ 'tool-icon-planned': planned }">
    <svg viewBox="0 0 24 24" aria-hidden="true">
      <!-- jeté: a leap -->
      <template v-if="name === 'jete'">
        <path d="M2.5 20 Q12 -5 21.5 20" stroke-dasharray="2.2 2.4" />
        <circle cx="12" cy="7.5" r="2.6" fill="currentColor" stroke="none" />
        <path d="M1.5 21.5 H22.5" />
      </template>
      <!-- jete-agent: a terminal inside the µVM -->
      <template v-else-if="name === 'jete-agent'">
        <rect x="3" y="4" width="18" height="16" rx="2.5" />
        <path d="M7 10 L10 12.5 L7 15" />
        <path d="M12.5 15.5 H17" />
      </template>
      <!-- barre: the rail dancers hold -->
      <template v-else-if="name === 'barre'">
        <path d="M2.5 8.5 H21.5" stroke-width="2.6" />
        <path d="M6 8.5 V20 M18 8.5 V20" />
        <path d="M4 20 H8 M16 20 H20" />
      </template>
      <!-- mime: two speech bubbles -->
      <template v-else-if="name === 'mime'">
        <path d="M3 5.5 H14 A1.5 1.5 0 0 1 15.5 7 V12 A1.5 1.5 0 0 1 14 13.5 H8 L5 16.5 V13.5 H4.5 A1.5 1.5 0 0 1 3 12 Z" />
        <path d="M18.5 9.5 H19.5 A1.5 1.5 0 0 1 21 11 V16 A1.5 1.5 0 0 1 19.5 17.5 H19 V20.5 L16 17.5 H11 A1.5 1.5 0 0 1 9.5 16 V16" />
      </template>
      <!-- regisseur: the cue sheet -->
      <template v-else-if="name === 'regisseur'">
        <rect x="5" y="4.5" width="14" height="17" rx="2" />
        <path d="M9.5 3 H14.5 V6 H9.5 Z" />
        <path d="M8.5 11 L9.7 12.2 L11.8 10 M13.5 11 H16 M8.5 16 L9.7 17.2 L11.8 15 M13.5 16 H16" />
      </template>
    </svg>
  </span>
</template>
