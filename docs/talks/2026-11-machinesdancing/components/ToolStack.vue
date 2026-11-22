<script setup lang="ts">
// What each host tool is built on, layer by layer, from the code:
// crates/jete/src/{vm,net,agent}.rs, crates/barre/src/{serve,policy}.rs,
// ansible/roles/barre/templates/barre.service.j2.
// <ToolStack /> is the whole stack, one column per tool with the layers
// aligned across them; <ToolStack tool="jete" /> is one tool's column.
const props = defineProps<{ tool?: 'jete' | 'barre' }>()

const tools = [
  { name: 'jete', role: 'µVM manager', state: 'built' },
  { name: 'barre', role: 'egress gateway', state: 'built' },
  { name: 'mime', role: 'message bus', state: 'planned' },
] as const

const layers = [
  {
    label: 'Isolation',
    jete: '<b>Firecracker</b> + <b>jailer</b> on <b>KVM</b>: chroot, cgroup v2, PID namespace, a uid per µVM',
    barre: '<b>systemd</b> sandbox: own user, no capabilities, seccomp, read-only <code>/</code>',
  },
  {
    label: 'Network',
    jete: 'a <b>tap</b> per µVM (<code>ip tuntap</code>) · <b>nftables</b>: one table, <code>inet jete</code>',
    barre: 'jete\'s <b>nftables redirect</b> → <code>SO_ORIGINAL_DST</code> · DNS on UDP 53 · TLS SNI, HTTP Host',
  },
  {
    label: 'Disks, state',
    jete: '<b>XFS reflinks</b> (<code>FICLONE</code>) · <b>pidfd</b> to stop · <b>flock</b> to allocate',
    barre: 'policy files in <code>/run/barre</code>, re-read every 0.5 s · JSON lines → <b>journald</b>',
  },
  {
    label: 'To the guest',
    jete: '<b>vsock</b>, through Firecracker\'s Unix socket, to jete-agent',
    barre: 'the <b>tap</b>: all of the µVM\'s TCP, and UDP 53',
    mime: '<b>vsock</b>',
  },
]

const rust = 'Rust, static musl binaries · <b>tokio</b> · <b>rustix</b>'
const linux = '<b>Linux 7.0</b> · KVM · Ubuntu Server 26.04'
const hardware = 'hemlock: i5-10500T, 16 GB'

const one = props.tool ? tools.find(t => t.name === props.tool)! : undefined
</script>

<template>
  <!-- One tool, as a column -->
  <div v-if="one" class="ts-col">
    <template v-for="l in layers" :key="l.label">
      <div class="ts-label">{{ l.label }}</div>
      <div class="ts-box" v-html="l[one.name]" />
    </template>
    <div class="ts-label">Code</div>
    <div class="ts-box" v-html="rust" />
    <div class="ts-label">Host</div>
    <div class="ts-box" v-html="linux" />
  </div>

  <!-- The whole stack: layers aligned across the tools, the µVM beside them -->
  <div v-else class="ts-grid">
    <div />
    <div class="ts-box ts-planned ts-span3 ts-tool">
      <ToolIcon name="regisseur" />
      <div><b>regisseur</b> · control plane: roles, tickets, workflows; spawns agents</div>
    </div>
    <div class="ts-guest-title">Inside each µVM</div>

    <div />
    <div v-for="t in tools" :key="t.name" class="ts-box ts-tool" :class="`ts-${t.state}`">
      <ToolIcon :name="t.name" />
      <div><b>{{ t.name }}</b><div class="ts-role">{{ t.role }}</div></div>
    </div>
    <div class="ts-guest">
      <div class="ts-box"><span>Claude Code, the agent program</span></div>
      <div class="ts-box ts-planned"><span>mime MCP server</span></div>
      <div class="ts-box ts-built ts-tool"><ToolIcon name="jete-agent" /><div><b>jete-agent</b><div class="ts-role">vsock server: exec, pty, ready</div></div></div>
      <div class="ts-box"><span>Ubuntu 26.04 · systemd · <b>Linux 6.18</b></span></div>
      <div class="ts-box"><span>Firecracker's virtual machine: virtio block, net, vsock</span></div>
    </div>

    <template v-for="l in layers" :key="l.label">
      <div class="ts-label">{{ l.label }}</div>
      <div class="ts-box" :class="{ 'ts-empty': !l.jete }" v-html="l.jete ?? ''" />
      <div class="ts-box" :class="{ 'ts-empty': !l.barre }" v-html="l.barre ?? ''" />
      <div class="ts-box" :class="l.mime ? 'ts-planned' : 'ts-empty'" v-html="l.mime ?? ''" />
    </template>

    <div class="ts-label">Code</div>
    <div class="ts-box ts-span2" v-html="rust" />
    <div class="ts-box ts-empty" />
    <div class="ts-label">Host</div>
    <div class="ts-box ts-span3" v-html="linux" />
    <div class="ts-label">Hardware</div>
    <div class="ts-box ts-span3" v-html="hardware" />
  </div>
</template>

<style scoped>
.ts-box { border: 1px solid #9ca3af; border-radius: 6px; padding: 0.25rem 0.5rem; background: #fff; line-height: 1.3; }
.ts-built { background: #d1fae5; border-color: #059669; color: #064e3b; }
.ts-planned { background: #f3f4f6; border-color: #9ca3af; border-style: dashed; color: #6b7280; }
.ts-empty { border: none; background: none; }
.ts-tool { display: flex; align-items: center; gap: 0.5rem; }
.ts-role { font-size: 0.85em; opacity: 0.8; }
.ts-label { font-size: 0.85em; text-transform: uppercase; letter-spacing: 0.04em; color: #6b7280; align-self: center; }
.ts-box :deep(code) { font-size: 0.95em; padding: 0 0.15em; }
.ts-box :deep(.tool-icon) { width: 1.9rem; height: 1.9rem; flex: none; }
.ts-box :deep(.tool-icon svg) { width: 1.25rem; height: 1.25rem; }

.ts-grid { display: grid; grid-template-columns: 5.2rem 1fr 1fr 6.2rem 11rem; gap: 0.3rem 0.4rem; font-size: 0.68rem; }
.ts-span2 { grid-column: span 2; }
.ts-span3 { grid-column: span 3; }
.ts-guest-title { font-weight: 700; padding-left: 0.8rem; align-self: end; }
.ts-guest { grid-column: 5; grid-row: 2 / span 8; display: flex; flex-direction: column; gap: 0.3rem; padding-left: 0.8rem; border-left: 2px dashed #9ca3af; }
.ts-guest .ts-box { flex: 1; display: flex; align-items: center; }

.ts-col { display: flex; flex-direction: column; gap: 0.15rem; font-size: 0.78rem; padding-top: 0.3rem; }
.ts-col .ts-label { margin-top: 0.5rem; font-size: 0.8em; align-self: start; }
</style>
