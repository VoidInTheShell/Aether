import apiClient from './client'

/**
 * Codex Turn-State 复用模块 API。
 *
 * 对应后端模块 `codex_turn_state`（规划中）：在 (账号, 模型) 桶内复用官方
 * Codex `X-Codex-Turn-State`，采到正常态（292）存库，业务请求发往上游前装上，
 * 绕开降级态（312）。四条硬规则：
 *   1. 绝不跨账号复用
 *   2. 绝不跨模型复用
 *   3. 同账号同模型可跨 IP
 *   4. TTL 从令牌自带 Fernet 签发时间戳起算（默认 3600s）
 *
 * 注意：桶值本身永远不下发前端，接口只暴露状态/长度/时间戳。
 */

// ========== 类型 ==========

/** 桶值来源：主动探测 / 业务流量被动采集 */
export type TurnStateHarvestSource = 'probe' | 'passive'

export interface TurnStateBucket {
  key_id: string
  key_name: string
  /** 上游官方模型名（带横线，如 gpt-5.5） */
  model: string
  /** 桶内是否有未过期的正常态（292） */
  ready: boolean
  /** 剩余有效期（秒），从令牌自带签发时间戳起算；空桶为 null */
  ttl_remaining_seconds: number | null
  issued_at_unix: number | null
  expires_at_unix: number | null
  source: TurnStateHarvestSource | null
  /** 最近一次采集使用的出口（静态池 URL 脱敏 / 轮换池标记 / direct） */
  last_exit: string | null
}

export interface TurnStateCounters {
  /** 入库（采到 292） */
  harvest: number
  /** 312 → 292 替换 */
  substitute: number
  /** 无头请求装头 */
  inject: number
  /** 不动（桶空/过期/键不全之外的原样放行） */
  pass: number
  /** 桶键不全跳过 */
  skip: number
}

export interface TurnStateProbeRun {
  running: boolean
  started_at_unix: number | null
  finished_at_unix: number | null
  /** 本轮目标桶数（账号 × 模型） */
  total: number
  done: number
  /** 探测日志（只含状态与长度，绝不含值） */
  lines: string[]
}

/**
 * 账号级降智判定（聚合 (账号 × 模型) 桶的探测结果）：
 * normal = 正常；suspected = 有桶连续采到 312 但未达阈值；degraded = 达到
 * degrade_threshold，已按 degrade_action 处置。采到 292 自动恢复 normal。
 */
export type TurnStateAccountVerdict = 'normal' | 'suspected' | 'degraded'

export interface TurnStateAccountHealth {
  key_id: string
  key_name: string
  verdict: TurnStateAccountVerdict
  /** 连续多少轮探测全出口 312；采到 292 归零 */
  consecutive_degraded_rounds: number
  /** 当前采不到正常态的模型（空桶且探测失败） */
  degraded_models: string[]
  last_probe_at_unix: number | null
  /** 被判 degraded 的起始时间；未判定为 null */
  degraded_since_unix: number | null
}

export interface TurnStateStatus {
  enabled: boolean
  dry_run: boolean
  buckets: TurnStateBucket[]
  /** 账号级降智判定：当前是否被降智看这里，桶矩阵看明细 */
  accounts: TurnStateAccountHealth[]
  counters: TurnStateCounters
  counters_since_unix: number
  probe_run: TurnStateProbeRun
}

export interface TurnStateScope {
  /** 参与主动探测的号池 key_id 列表（空 = 不探测） */
  key_ids: string[]
  /** 目标模型清单（必须带横线） */
  models: string[]
  /** 静态出口池：一条 URL = 一个固定 IP，每桶每条 55 分钟一次机会 */
  probe_proxies: string[]
  /** 轮换出口池：一条 URL = 住宅网关，每次连接换地址，每桶同一条可连试 */
  probe_proxies_rotating: string[]
}

/** 装头策略：仅当请求带降级态（312）才替换 / 桶里有未过期正常态就装上 */
export type TurnStateInjectMode = 'replace-only' | 'always'

/** 判定账号级降智后的处置：只换头不动账号 / 写入健康分降权 / 冷却禁用 */
export type TurnStateDegradeAction = 'none' | 'downweight' | 'disable'

export interface TurnStateConfig {
  /** 装头策略：replace-only = 仅当请求带 312 时替换；always = 桶里有就装头 */
  inject_mode: TurnStateInjectMode
  /** 从业务流量被动采集（响应里带回的正常态直接入库），默认开 */
  harvest_inband: boolean
  /** 判定账号级降智后的动作；判定后采到 292 会自动恢复 */
  degrade_action: TurnStateDegradeAction
  /** 连续 N 轮所有出口均 312 才判定账号级降智，默认 3 */
  degrade_threshold: number
  /** 桶有效期（秒），从令牌自带签发时间戳起算；修改会清空全部已采桶 */
  ttl_seconds: number
  /** 正常态（模板）头长度指纹，默认 292；修改会清空全部已采桶 */
  template_length: number
  /** 降级态头长度指纹，默认 312；修改会清空全部已采桶 */
  replace_length: number
  /** 到期前自动续期 */
  auto_renew: boolean
  /** 剩余 TTL 低于该值（秒）时触发续期探测 */
  renew_threshold_seconds: number
  /** 探测失败后同一账号的退避时间（秒） */
  account_backoff_seconds: number
  /** 单条静态出口对同一桶的冷却（秒），默认 3300（55 分钟） */
  exit_cooldown_seconds: number
  /** 轮换池对同一桶最多连试次数 */
  rotating_max_attempts: number
  /** 同时在探测的账号数上限 */
  max_accounts_in_flight: number
  /** 轮换池整池冷却（秒），默认 600（10 分钟） */
  rotating_cooldown_seconds: number
  /** 出口网络错误的短冷却（秒），默认 300 */
  network_cooldown_seconds: number
  /** 同一账号两次上游探测的最小间隔（秒），默认 2 */
  probe_account_pace_seconds: number
  /** 代理可用性检查的单次请求超时（秒），默认 8 */
  proxy_check_timeout_seconds: number
  /** 代理检查的并发数，默认 6 */
  proxy_check_concurrency: number
  /** 代理检查的总时间预算（秒），超时条目标记为预算耗尽，默认 45 */
  proxy_check_total_budget_seconds: number
}

export type TurnStateProxyPool = 'static' | 'rotating'

export interface TurnStateProxyCheckItem {
  proxy: string
  pool: TurnStateProxyPool
  /** 能否到达上游（不带凭据，上游回 401 即通） */
  reachable: boolean
  /** 上游状态码（401 通 / 403 被拒 / 429 限速），连不上为 null */
  status_code: number | null
  detail: string
  exit_ip: string | null
  country: string | null
  cf_colo: string | null
  /** 放错池子等可被证伪的告警；不可证伪的方向不报 */
  warning: string | null
}

// ========== API ==========

const BASE = '/api/admin/modules/codex-turn-state'

export const codexTurnStateApi = {
  /** 模块状态：桶矩阵 + 决策计数 + 探测运行态 */
  async getStatus(): Promise<TurnStateStatus> {
    const response = await apiClient.get<TurnStateStatus>(`${BASE}/status`)
    return response.data
  },

  /** 探测范围（存模块自己的 probe-scope，覆盖全局配置） */
  async getScope(): Promise<TurnStateScope> {
    const response = await apiClient.get<TurnStateScope>(`${BASE}/scope`)
    return response.data
  },

  async updateScope(scope: TurnStateScope): Promise<TurnStateScope> {
    const response = await apiClient.put<TurnStateScope>(`${BASE}/scope`, scope)
    return response.data
  },

  /**
   * 启动探测。可传 key_ids 定向探测单个账号（操作员手动触发，不受探测范围限制），
   * 不传则按探测范围全量巡检。队列按账号判定排序：降智 > 疑似 > 正常，降智账号优先恢复。
   */
  async startProbe(keyIds?: string[]): Promise<TurnStateProbeRun> {
    const response = await apiClient.post<TurnStateProbeRun>(`${BASE}/probe/start`, keyIds?.length ? { key_ids: keyIds } : {})
    return response.data
  },

  async cancelProbe(): Promise<TurnStateProbeRun> {
    const response = await apiClient.post<TurnStateProbeRun>(`${BASE}/probe/cancel`)
    return response.data
  },

  /** 逐条测代理能否到上游（不带凭据，不花额度）。测的是已保存的池子 */
  async proxyCheck(): Promise<TurnStateProxyCheckItem[]> {
    const response = await apiClient.post<TurnStateProxyCheckItem[]>(`${BASE}/proxy-check`)
    return response.data
  },

  async setDryRun(dryRun: boolean): Promise<{ dry_run: boolean }> {
    const response = await apiClient.put<{ dry_run: boolean }>(`${BASE}/dry-run`, { dry_run: dryRun })
    return response.data
  },

  /** 清空桶（下次业务请求由被动采集重新接管） */
  async clearBuckets(): Promise<{ cleared: number }> {
    const response = await apiClient.post<{ cleared: number }>(`${BASE}/clear`)
    return response.data
  },

  /** 模块策略配置：注入策略、降智处置、TTL 与探测高级参数 */
  async getConfig(): Promise<TurnStateConfig> {
    const response = await apiClient.get<TurnStateConfig>(`${BASE}/config`)
    return response.data
  },

  /** 保存配置；修改 ttl_seconds / template_length / replace_length 会清空全部已采桶 */
  async updateConfig(config: TurnStateConfig): Promise<TurnStateConfig> {
    const response = await apiClient.put<TurnStateConfig>(`${BASE}/config`, config)
    return response.data
  },
}
