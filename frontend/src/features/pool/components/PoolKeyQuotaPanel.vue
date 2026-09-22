<template>
  <div
    v-if="variant === 'mobile'"
    class="rounded-xl border border-border/50 bg-muted/30 px-3 py-2 text-xs"
  >
    <div class="text-muted-foreground mb-1">
      {{ legacyT('配额') }}
    </div>
    <div
      v-if="items.length"
      :class="hasNumericOnlyItems ? '' : 'space-y-2'"
    >
      <QuotaProgressRows
        :items="items"
        mobile
      />
      <div
        v-if="accountQuotaText"
        class="text-[10px] leading-none text-muted-foreground tabular-nums"
      >
        {{ accountQuotaText }}
      </div>
      <ResetCredits />
    </div>
    <div
      v-else-if="accountQuotaText || fallbackText"
      :class="textClass"
    >
      {{ accountQuotaText || fallbackText }}
    </div>
    <div
      v-else-if="!turnStateItems.length"
      class="text-muted-foreground"
    >
      -
    </div>
    <TurnStateRows mobile />
  </div>

  <template v-else>
    <div
      v-if="items.length"
      class="w-full max-w-[208px]"
      :class="hasNumericOnlyItems ? '' : 'space-y-2'"
    >
      <QuotaProgressRows :items="items" />
      <div
        v-if="accountQuotaText"
        class="text-[10px] leading-none text-muted-foreground tabular-nums"
      >
        {{ accountQuotaText }}
      </div>
      <ResetCredits />
      <TurnStateRows />
    </div>
    <template v-else>
      <span
        v-if="accountQuotaText || fallbackText"
        :class="textClass"
      >
        {{ accountQuotaText || fallbackText }}
      </span>
      <span
        v-else-if="!turnStateItems.length"
        class="text-xs text-muted-foreground"
      >-</span>
      <TurnStateRows />
    </template>
  </template>
</template>

<script setup lang="ts">
import { computed, defineComponent, h, type PropType } from 'vue'
import { useI18n } from '@/i18n'

export interface PoolQuotaProgressDisplayItem {
  label: string
  remainingPercent: number
  resetText: string
  meterText: string
  barClass: string
  meterClass: string
  numericOnly?: boolean
}

/** Codex Turn-State 桶状态展示项（codex_turn_state 模块，按模型一格） */
export interface PoolTurnStateDisplayItem {
  model: string
  ready: boolean
  /** 已格式化的剩余有效期，如 "47 分钟"；空桶为 null */
  ttlText: string | null
  /** 该模型被探测到降智（312）且未恢复 */
  degraded?: boolean
  /** 上游拒绝该模型（探测 400/404），不可能有模板 */
  unsupported?: boolean
}

const props = withDefaults(defineProps<{
  items: PoolQuotaProgressDisplayItem[]
  accountQuotaText?: string | null
  fallbackText?: string | null
  textClass?: string
  variant?: 'desktop' | 'mobile'
  resetCreditText?: string | null
  resetCreditItems?: string[]
  canConsumeResetCredit?: boolean
  consumingResetCredit?: boolean
  turnStateItems?: PoolTurnStateDisplayItem[]
}>(), {
  accountQuotaText: null,
  fallbackText: null,
  textClass: '',
  variant: 'desktop',
  resetCreditText: null,
  resetCreditItems: () => [],
  canConsumeResetCredit: false,
  consumingResetCredit: false,
  turnStateItems: () => [],
})

const emit = defineEmits<{
  'consume-reset-credit': []
}>()

const { legacyT } = useI18n()
const hasNumericOnlyItems = computed(() => props.items.length > 0 && props.items.every(item => item.numericOnly))

const ResetCredits = defineComponent({
  name: 'PoolQuotaResetCredits',
  setup() {
    return () => props.resetCreditText ? h('div', {
      'data-testid': 'pool-quota-reset-credits',
      class: 'mt-2 border-t border-border/50 pt-1.5 text-[10px] leading-4 text-muted-foreground',
    }, [
      h('div', { class: 'flex flex-wrap items-center gap-x-1' }, [
        props.canConsumeResetCredit
          ? h('button', {
            type: 'button',
            disabled: props.consumingResetCredit,
            class: 'font-medium text-primary hover:underline disabled:pointer-events-none disabled:opacity-60',
            onClick: () => emit('consume-reset-credit'),
          }, props.consumingResetCredit ? legacyT('重置中...') : legacyT('点击以进行重置'))
          : null,
        h('span', props.resetCreditText),
      ]),
      props.resetCreditItems.length
        ? h('div', { class: 'truncate tabular-nums', title: props.resetCreditItems.join(' · ') }, props.resetCreditItems.join(' · '))
        : null,
    ]) : null
  },
})

const TurnStateRows = defineComponent({
  name: 'PoolTurnStateRows',
  props: {
    mobile: {
      type: Boolean,
      default: false,
    },
  },
  setup() {
    const countedItems = () => props.turnStateItems.filter(item => !item.unsupported)
    const countedReady = () => countedItems().filter(item => item.ready).length
    return () => props.turnStateItems.length ? h('div', {
      'data-testid': 'pool-turn-state-rows',
      class: 'mt-2 border-t border-border/50 pt-1.5 text-[10px] leading-4 text-muted-foreground',
    }, [
      h('div', { class: 'mb-1 flex items-center justify-between' }, [
        h('span', legacyT('Turn-State')),
        h('span', { class: 'tabular-nums' }, `${countedReady()}/${countedItems().length} ${legacyT('就绪')}${props.turnStateItems.some(item => item.unsupported) ? ` · ${props.turnStateItems.filter(item => item.unsupported).length} ${legacyT('不支持')}` : ''}`),
      ]),
      h('div', { class: 'flex flex-wrap gap-1' }, props.turnStateItems.map(item => {
        // 绿 = 有可用模板；红 = 该模型降智且无模板；灰红 = 无模板但未见降智；灰 = 上游不支持
        const chipClass = item.unsupported
          ? 'border-border/60 bg-muted/40 text-muted-foreground/70'
          : item.ready
            ? 'border-emerald-500/30 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400'
            : item.degraded
              ? 'border-destructive/40 bg-destructive/10 text-destructive'
              : 'border-rose-500/30 bg-rose-500/5 text-rose-600 dark:text-rose-400'
        const title = item.unsupported
          ? `${item.model} · ${legacyT('上游不支持该模型（探测 400）')}`
          : item.ready
            ? `${item.model} · ${legacyT('剩余')} ${item.ttlText}`
            : item.degraded
              ? `${item.model} · ${legacyT('被探测到降智，暂无可用模板')}`
              : `${item.model} · ${legacyT('暂无模板')}`
        const mark = item.unsupported ? '⊘' : item.ready ? '✓' : item.degraded ? '✗' : '—'
        return h('span', {
          key: item.model,
          'data-testid': 'pool-turn-state-chip',
          title,
          class: [
            'inline-flex items-center gap-1 rounded-md border px-1.5 py-0.5 font-mono tabular-nums',
            chipClass,
          ],
        }, `${shortModelName(item.model)} ${mark}${item.ready && item.ttlText ? ` ${item.ttlText}` : ''}`)
      })),
    ]) : null
  },
})

function shortModelName(model: string): string {
  return model.replace(/^gpt-/, '')
}

const QuotaProgressRows = defineComponent({
  name: 'QuotaProgressRows',
  props: {
    items: {
      type: Array as PropType<PoolQuotaProgressDisplayItem[]>,
      required: true,
    },
    mobile: {
      type: Boolean,
      default: false,
    },
  },
  setup(props) {
    return () => h('div', {
      'data-testid': 'pool-quota-rows',
      class: props.items.every(item => item.numericOnly)
        ? 'grid grid-cols-2 gap-x-3 gap-y-1.5 min-w-0'
        : 'space-y-2',
    }, props.items.map((item, idx) => h('div', {
      key: `${item.label}-${idx}`,
      class: item.numericOnly
        ? 'flex min-w-0 items-baseline justify-between gap-2 text-[10px] leading-4'
        : props.mobile
          ? 'flex flex-col gap-1 min-w-0'
          : 'flex flex-col gap-1 min-w-[140px] max-w-[208px]',
    }, [
      h('div', { class: item.numericOnly ? 'contents' : 'flex items-center justify-between text-[10px] leading-none' }, [
        h('span', {
          'data-testid': 'pool-quota-period-label',
          class: item.numericOnly
            ? 'min-w-0 truncate text-muted-foreground'
            : 'text-muted-foreground font-medium shrink-0',
          title: item.numericOnly ? item.label : undefined,
        }, item.label),
        item.resetText && !item.numericOnly
          ? h('span', {
            'data-testid': 'pool-quota-reset-text',
            class: 'text-muted-foreground/80 tabular-nums truncate',
            title: item.resetText,
          }, item.resetText)
          : null,
      ]),
      h('div', { class: item.numericOnly ? 'contents' : 'flex items-center gap-1.5' }, [
        item.numericOnly
          ? null
          : h('div', {
            'data-testid': 'pool-quota-progress-track',
            class: 'relative flex-1 h-1.5 rounded-full bg-border overflow-hidden',
          }, [
            h('div', {
              class: ['absolute left-0 top-0 h-full rounded-full transition-all duration-300', item.barClass],
              style: { width: `${item.remainingPercent}%` },
            }),
          ]),
        h('span', {
          'data-testid': 'pool-quota-meter-text',
          class: [
            'shrink-0 text-[10px] font-medium tabular-nums leading-none',
            item.numericOnly ? 'text-right' : '',
            item.meterClass,
          ],
        }, item.meterText),
      ]),
    ])))
  },
})
</script>
