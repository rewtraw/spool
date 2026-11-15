<script lang="ts">
  // A secret setting: never shown once saved. Typing replaces it; Clear forgets it.
  let { label, set = false, value = $bindable(''), hint = '', cleared = $bindable(false) }: { label: string; set?: boolean; value?: string; hint?: string; cleared?: boolean } = $props();
</script>

<label class="field">{label}
  <div class="row">
    <input class="input mono grow" type="password" autocomplete="new-password" bind:value placeholder={cleared ? 'Cleared when saved' : set ? 'Saved; type to replace it' : 'None'} oninput={() => (cleared = false)} />
    {#if set && !cleared && !value}<button class="btn small ghost" type="button" onclick={() => (cleared = true)}>Clear</button>{/if}
  </div>
  {#if hint}<span class="hint">{hint}</span>{/if}
</label>
