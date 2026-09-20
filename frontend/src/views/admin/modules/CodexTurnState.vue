<template>
  <PageContainer>
    <PageHeader
      title="Codex 状态复用"
      description="在 (账号, 模型) 桶内复用官方 Codex Turn-State：采到正常态存库，业务请求发往上游前装上，绕开降级态（降智）。"
      :icon="Recycle"
    >
      <template #actions>
        <Button
          variant="outline"
          :disabled="loading || saving"
          @click="loadAll"
        >
          <RefreshCw
            class="mr-2 h-4 w-4"
            :class="{ 'animate-spin': loading }"
          />
          刷新
        </Button>
        <Button
          :disabled="loading || saving || !hasConfigChanges"
          @click="saveConfig"
        >
          {{ saving ? '保存中...' : '保存配置' }}
        </Button>
      </template>
    </PageHeader>

    <div class="mt-6 space-y-6">
      <!-- 总览：模块开关 + Dry-Run + 决策计数 + 探测运行 -->
      <section class="rounded-2xl border border-border bg-card p-5">
        <div class="flex flex-col gap-4 lg:flex-row lg:items-center lg:justify-between">
          <div class="space-y-1">
            <div class="flex items-center gap-2">
              <span
                class="h-2.5 w-2.5 rounded-full ring-2 ring-offset-2 ring-offset-background"
                :class="status?.enabled ? 'bg-emerald-500 ring-emerald-500/30' : 'bg-muted-foreground/40 ring-muted/40'"
              />
              <p class="text-sm font-semibold text-foreground">
                {{ status?.enabled ? '运行中' : '未启用' }}
                <span
                  v-if="status?.dry_run"
                  class="ml-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-1.5 py-0.5 text-[11px] font-medium text-amber-600 dark:text-amber-400"
                >Dry-Run：照走照记但不真改头</span>
              </p>
            </div>
            <p class="max-w-3xl text-sm text-muted-foreground">
              桶值本身永不离开服务端，这里只展示状态、时间与长度。
            </p>
          </div>
          <div class="flex flex-wrap items-center gap-3">
            <div class="flex items-center gap-3 rounded-xl border border-border bg-muted/40 px-4 py-3">
              <p class="text-sm font-medium text-foreground">
                启用模块
              </p>
              <Switch
                :model-value="status?.enabled ?? false"
                :disabled="!status || moduleToggling"
                @update:model-value="toggleModuleEnabled"
              />
            </div>
            <div class="flex items-center gap-3 rounded-xl border border-border bg-muted/40 px-4 py-3">
              <p class="text-sm font-medium text-foreground">
                Dry-Run
              </p>
              <Switch
                :model-value="status?.dry_run ?? false"
                :disabled="!status || dryRunSaving"
                @update:model-value="toggleDryRun"
              />
            </div>
          </div>
        </div>

        <div class="mt-4 grid gap-4 lg:grid-cols-2">
          <!-- 决策计数 -->
          <div class="rounded-xl border border-border bg-muted/40 px-4 py-3">
            <div class="flex flex-wrap items-center gap-2">
              <template
                v-for="item in counterItems"
                :key="item.label"
              >
                <div class="px-2 text-center">
                  <p class="text-base font-semibold tabular-nums text-foreground">
                    {{ item.value }}
                  </p>
                  <p class="text-[10px] text-muted-foreground">
                    {{ item.label }}
                  </p>
                </div>
              </template>
            </div>
            <p
              v-if="status?.counters_since_unix"
              class="mt-2 border-t border-border/60 pt-2 text-[11px] text-muted-foreground"
            >
              自 {{ formatTime(status.counters_since_unix) }} 起累计；harvest 入库 / substitute 替换 / inject 装头 / pass 放行 / skip 跳过。
            </p>
          </div>

          <!-- 探测运行 -->
          <div class="space-y-3">
            <div class="flex flex-wrap items-center gap-2">
              <Button
                v-if="!probeRun?.running"
                size="sm"
                :disabled="loading || probeActionPending"
                @click="startProbe"
              >
                <Play class="mr-2 h-4 w-4" />
                开始探测
              </Button>
              <Button
                v-else
                size="sm"
                variant="outline"
                :disabled="probeActionPending"
                @click="cancelProbe"
              >
                <Square class="mr-2 h-4 w-4" />
                取消探测
              </Button>
              <span
                v-if="probeRun && (probeRun.running || probeRun.total > 0)"
                class="text-xs tabular-nums text-muted-foreground"
              >
                {{ probeRun.running ? `探测中 ${probeRun.done}/${probeRun.total}` : `上轮探测 ${probeRun.done}/${probeRun.total}` }}
              </span>
            </div>
            <div
              v-if="probeRun && (probeRun.running || probeRun.total > 0)"
              class="relative h-1.5 overflow-hidden rounded-full bg-border"
            >
              <div
                class="absolute left-0 top-0 h-full rounded-full bg-primary transition-all duration-500"
                :style="{ width: `${probeRun.total ? (probeRun.done / probeRun.total) * 100 : 0}%` }"
              />
            </div>
            <div
              v-if="probeRun && (probeRun.running || probeRun.lines.length)"
              class="rounded-xl border border-border"
            >
              <div class="flex items-center justify-between border-b border-border bg-muted/40 px-3 py-2 text-xs text-muted-foreground">
                <span>探测日志（只含状态与长度，绝不含值）</span>
                <span v-if="probeRun.started_at_unix">{{ formatTime(probeRun.started_at_unix) }}</span>
              </div>
              <pre class="max-h-44 overflow-auto whitespace-pre-wrap break-words px-3 py-2 font-mono text-[11px] leading-5 text-muted-foreground">{{ probeRun.lines.join('\n') || '等待输出...' }}</pre>
            </div>
          </div>
        </div>
      </section>

      <!-- 账号降智状态 -->
      <CardSection
        title="账号降智状态"
        description="按账号聚合探测结论：连续 N 轮全出口 312 判定降智（N 见下方「降智处置策略」），任一模型重新采到正常态立即自动恢复。探测队列按 降智 > 疑似 > 正常 排序，也可对单个账号手动「立即探测」。"
      >
        <div
          v-if="!loading && !accounts.length"
          class="rounded-xl border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground"
        >
          暂无账号数据——先在「账号与模型」导入并勾选账号，再跑一轮探测。
        </div>
        <div
          v-else
          class="overflow-x-auto rounded-xl border border-border"
        >
          <table class="min-w-[920px] w-full text-sm">
            <thead class="bg-muted/50 text-left text-xs font-medium text-muted-foreground">
              <tr>
                <th class="px-4 py-3">
                  账号
                </th>
                <th class="w-[110px] px-4 py-3">
                  当前判定
                </th>
                <th class="w-[90px] px-4 py-3">
                  连续轮次
                </th>
                <th class="px-4 py-3">
                  采不到正常态的模型
                </th>
                <th class="w-[150px] px-4 py-3">
                  最近探测
                </th>
                <th class="w-[150px] px-4 py-3">
                  降智始于
                </th>
                <th class="w-[110px] px-4 py-3">
                  当前处置
                </th>
                <th class="w-[84px] px-4 py-3">
                  操作
                </th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="account in pagedAccounts"
                :key="account.key_id"
                class="border-t border-border"
              >
                <td class="px-4 py-3">
                  <p class="font-medium text-foreground">
                    {{ account.key_name }}
                  </p>
                  <p class="font-mono text-[11px] text-muted-foreground">
                    {{ account.key_id }}
                  </p>
                </td>
                <td class="px-4 py-3">
                  <span class="flex items-center gap-1.5">
                    <span
                      class="h-2 w-2 rounded-full"
                      :class="verdictMeta[account.verdict].dotClass"
                    />
                    <span
                      class="text-xs font-medium"
                      :class="verdictMeta[account.verdict].textClass"
                    >{{ verdictMeta[account.verdict].label }}</span>
                  </span>
                </td>
                <td class="px-4 py-3 text-xs tabular-nums text-muted-foreground">
                  {{ account.consecutive_degraded_rounds }} / {{ config.degrade_threshold }}
                </td>
                <td class="px-4 py-3">
                  <div
                    v-if="account.degraded_models.length"
                    class="flex flex-wrap gap-1"
                  >
                    <span
                      v-for="model in account.degraded_models"
                      :key="model"
                      class="rounded-md border border-amber-500/40 bg-amber-500/10 px-1.5 py-0.5 font-mono text-[10px] text-amber-600 dark:text-amber-400"
                    >{{ model }}</span>
                  </div>
                  <span
                    v-else
                    class="text-xs text-muted-foreground"
                  >-</span>
                </td>
                <td class="px-4 py-3 text-[11px] tabular-nums text-muted-foreground">
                  {{ account.last_probe_at_unix ? formatTime(account.last_probe_at_unix) : '-' }}
                </td>
                <td class="px-4 py-3 text-[11px] tabular-nums text-muted-foreground">
                  {{ account.degraded_since_unix ? formatTime(account.degraded_since_unix) : '-' }}
                </td>
                <td class="px-4 py-3">
                  <Badge
                    v-if="account.verdict === 'degraded'"
                    variant="outline"
                    class="border-destructive/40 text-[10px] text-destructive"
                  >
                    {{ degradeActionLabels[config.degrade_action] }}
                  </Badge>
                  <span
                    v-else
                    class="text-xs text-muted-foreground"
                  >-</span>
                </td>
                <td class="px-4 py-3">
                  <Button
                    variant="outline"
                    size="sm"
                    class="h-7 px-2 text-xs"
                    :disabled="loading || probeActionPending || probeRun?.running"
                    :title="`立即探测 ${account.key_name} 的所有模型，采到正常态即恢复`"
                    @click="startAccountProbe(account.key_id)"
                  >
                    <Play class="mr-1 h-3 w-3" />
                    探测
                  </Button>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <Pagination
          v-if="accounts.length"
          :current="accountPage"
          :total="accounts.length"
          :page-size="accountPageSize"
          cache-key="turn-state-accounts-page-size"
          @update:current="accountPage = $event"
          @update:page-size="accountPageSize = $event"
        />
      </CardSection>

      <!-- 桶矩阵 -->
      <CardSection
        title="桶矩阵"
        :description="`${readyBuckets.length} / ${buckets.length} 个桶持有未过期的正常态（292）；空桶由被动采集自愈，续期靠主动探测。桶值永不下发，只显示长度与时间。`"
      >
        <template #actions>
          <Button
            variant="outline"
            size="sm"
            class="text-destructive hover:text-destructive"
            :disabled="clearing || !buckets.length"
            @click="clearBuckets"
          >
            <Trash2 class="mr-2 h-4 w-4" />
            {{ clearing ? '清空中...' : '清空桶' }}
          </Button>
        </template>

        <div
          v-if="!loading && !buckets.length"
          class="rounded-xl border border-dashed border-border px-4 py-10 text-center"
        >
          <p class="text-sm font-medium text-foreground">
            还没有任何桶
          </p>
          <p class="mt-2 text-sm text-muted-foreground">
            先到下方「账号与模型」勾选账号和模型并保存，再回顶部点「开始探测」；开启被动采集后业务流量也会自动补桶。
          </p>
        </div>
        <div
          v-else
          class="overflow-x-auto rounded-xl border border-border"
        >
          <table class="min-w-[960px] w-full text-sm">
            <thead class="bg-muted/50 text-left text-xs font-medium text-muted-foreground">
              <tr>
                <th class="px-4 py-3">
                  账号
                </th>
                <th class="px-4 py-3">
                  模型
                </th>
                <th class="w-[90px] px-4 py-3">
                  状态
                </th>
                <th class="w-[220px] px-4 py-3">
                  TTL 剩余
                </th>
                <th class="w-[150px] px-4 py-3">
                  签发时间
                </th>
                <th class="w-[150px] px-4 py-3">
                  过期时间
                </th>
                <th class="w-[110px] px-4 py-3">
                  来源
                </th>
                <th class="px-4 py-3">
                  最近出口
                </th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="bucket in pagedBuckets"
                :key="`${bucket.key_id}:${bucket.model}`"
                class="border-t border-border"
              >
                <td class="px-4 py-3">
                  <p class="font-medium text-foreground">
                    {{ bucket.key_name }}
                  </p>
                  <p class="font-mono text-[11px] text-muted-foreground">
                    {{ bucket.key_id }}
                  </p>
                </td>
                <td class="px-4 py-3 font-mono text-xs">
                  {{ bucket.model }}
                </td>
                <td class="px-4 py-3">
                  <span class="flex items-center gap-1.5">
                    <span
                      class="h-2 w-2 rounded-full"
                      :class="bucket.ready ? 'bg-emerald-500' : 'bg-muted-foreground/40'"
                    />
                    <span
                      class="text-xs"
                      :class="bucket.ready ? 'text-emerald-600 dark:text-emerald-400' : 'text-muted-foreground'"
                    >{{ bucket.ready ? '就绪' : '空桶' }}</span>
                  </span>
                </td>
                <td class="px-4 py-3">
                  <div
                    v-if="bucket.ready && displayTtl(bucket) != null"
                    class="flex items-center gap-2"
                  >
                    <div class="relative h-1.5 flex-1 overflow-hidden rounded-full bg-border">
                      <div
                        class="absolute left-0 top-0 h-full rounded-full transition-all duration-300"
                        :class="ttlBarClass(displayTtl(bucket)!)"
                        :style="{ width: `${Math.min(100, (displayTtl(bucket)! / ttlTotalSeconds) * 100)}%` }"
                      />
                    </div>
                    <span
                      class="shrink-0 text-[11px] tabular-nums"
                      :class="ttlTextClass(displayTtl(bucket)!)"
                    >{{ formatTtl(displayTtl(bucket)!) }}</span>
                  </div>
                  <span
                    v-else
                    class="text-xs text-muted-foreground"
                  >-</span>
                </td>
                <td class="px-4 py-3 text-[11px] tabular-nums text-muted-foreground">
                  {{ bucket.issued_at_unix ? formatTime(bucket.issued_at_unix) : '-' }}
                </td>
                <td class="px-4 py-3 text-[11px] tabular-nums text-muted-foreground">
                  {{ bucket.expires_at_unix ? formatTime(bucket.expires_at_unix) : '-' }}
                </td>
                <td class="px-4 py-3">
                  <Badge
                    v-if="bucket.source"
                    variant="outline"
                    class="text-[10px]"
                    :class="bucket.source === 'probe' ? 'border-sky-500/40 text-sky-600 dark:text-sky-400' : 'border-violet-500/40 text-violet-600 dark:text-violet-400'"
                  >
                    {{ bucket.source === 'probe' ? 'probe' : 'passive' }}
                  </Badge>
                  <span
                    v-else
                    class="text-xs text-muted-foreground"
                  >-</span>
                </td>
                <td class="px-4 py-3 font-mono text-[11px] text-muted-foreground">
                  {{ bucket.last_exit || '-' }}
                </td>
              </tr>
              <tr v-if="loading && !buckets.length">
                <td
                  colspan="8"
                  class="px-4 py-10 text-center text-sm text-muted-foreground"
                >
                  加载中...
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <Pagination
          v-if="buckets.length"
          :current="bucketPage"
          :total="buckets.length"
          :page-size="bucketPageSize"
          cache-key="turn-state-buckets-page-size"
          @update:current="bucketPage = $event"
          @update:page-size="bucketPageSize = $event"
        />
      </CardSection>

      <!-- 账号与模型 -->
      <CardSection
        title="账号与模型"
        description="点「导入号池账号」拉取类型为 codex 的供应商号池账号，手动勾选参与探测的账号与模型。改了范围不用重启，续期循环每分钟重读一次。范围不是白名单：业务替换只看桶里有没有未过期的正常态。"
      >
        <template #actions>
          <Button
            variant="outline"
            size="sm"
            :disabled="loading || importingKeys"
            @click="importPoolKeys()"
          >
            <Download
              class="mr-2 h-4 w-4"
              :class="{ 'animate-pulse': importingKeys }"
            />
            {{ importingKeys ? '导入中...' : '导入号池账号' }}
          </Button>
          <Button
            size="sm"
            :disabled="loading || accountSaving || !accountCardDirty"
            @click="saveAccountScope"
          >
            {{ accountSaving ? '保存中...' : '保存账号与模型' }}
          </Button>
        </template>

        <div class="grid gap-6 lg:grid-cols-2">
          <div class="space-y-3">
            <p class="text-sm font-medium text-foreground">
              账号（{{ scope.key_ids.length }}/{{ availableKeys.length }}）
            </p>
            <div class="space-y-2 rounded-xl border border-border p-3">
              <label
                v-for="key in pagedScopeKeys"
                :key="key.key_id"
                class="flex cursor-pointer items-center gap-3 rounded-lg px-2 py-1.5 hover:bg-muted/50"
              >
                <Checkbox
                  :model-value="scope.key_ids.includes(key.key_id)"
                  @update:model-value="(checked) => toggleScopeKey(key.key_id, checked === true)"
                />
                <span class="min-w-0 flex-1">
                  <span class="block truncate text-sm text-foreground">{{ key.key_name }}</span>
                  <span class="block truncate font-mono text-[11px] text-muted-foreground">{{ key.key_id }}</span>
                </span>
                <Badge
                  v-if="key.provider_name && showKeyProviderName"
                  variant="outline"
                  class="shrink-0 text-[10px]"
                >
                  {{ key.provider_name }}
                </Badge>
                <Badge
                  v-if="key.missing"
                  variant="destructive"
                  class="shrink-0 text-[10px]"
                >
                  号池已删除
                </Badge>
              </label>
              <p
                v-if="!availableKeys.length"
                class="px-2 py-3 text-sm text-muted-foreground"
              >
                {{ keysImported ? 'codex 号池里没有账号' : '点右上角「导入号池账号」拉取 codex 号池账号' }}
              </p>
            </div>
            <Pagination
              v-if="availableKeys.length"
              :current="scopeKeyPage"
              :total="availableKeys.length"
              :page-size="scopeKeyPageSize"
              cache-key="turn-state-scope-keys-page-size"
              @update:current="scopeKeyPage = $event"
              @update:page-size="scopeKeyPageSize = $event"
            />
          </div>

          <div class="space-y-3">
            <p class="text-sm font-medium text-foreground">
              模型（{{ scope.models.length }}）
            </p>
            <div class="flex flex-wrap gap-2">
              <label
                v-for="model in availableModels"
                :key="model"
                class="flex cursor-pointer items-center gap-2 rounded-lg border border-border px-3 py-1.5 hover:bg-muted/50"
                :class="scope.models.includes(model) ? 'border-primary/50 bg-primary/5' : ''"
              >
                <Checkbox
                  :model-value="scope.models.includes(model)"
                  @update:model-value="(checked) => toggleScopeModel(model, checked === true)"
                />
                <span class="font-mono text-xs">{{ model }}</span>
              </label>
            </div>
            <div class="flex items-center gap-2">
              <Input
                v-model="newModelInput"
                size="sm"
                class="font-mono text-xs"
                placeholder="gpt-5.5-codex"
                @keydown.enter.prevent="addCustomModel"
              />
              <Button
                variant="outline"
                size="sm"
                :disabled="!newModelInput.trim()"
                @click="addCustomModel"
              >
                <Plus class="mr-1 h-4 w-4" />
                添加
              </Button>
            </div>
            <p class="text-[11px] leading-5 text-muted-foreground">
              可勾选已有模型，也可手动添加。必须是带横线的上游官方模型名（如 gpt-5.5-codex），别名无效。
            </p>
          </div>
        </div>
      </CardSection>

      <!-- 代理配置 -->
      <CardSection
        title="代理配置"
        description="主动探测用的出口池。改了不用重启，续期循环每分钟重读一次。放错池子是静默的，用下方「代理连通性检查」核对。"
      >
        <template #actions>
          <Button
            size="sm"
            :disabled="loading || proxySaving || !proxyCardDirty"
            @click="saveProxyScope"
          >
            {{ proxySaving ? '保存中...' : '保存代理配置' }}
          </Button>
        </template>

        <div class="grid gap-6 lg:grid-cols-2">
          <div>
            <div class="mb-1.5 flex items-baseline justify-between">
              <p class="text-sm font-medium text-foreground">
                静态出口池（{{ staticProxyLines.length }} 条）
              </p>
              <p class="text-[11px] text-muted-foreground">
                一条 URL = 一个固定 IP，每桶每条 55 分钟一次机会
              </p>
            </div>
            <Textarea
              v-model="staticProxyText"
              rows="5"
              class="font-mono text-xs"
              placeholder="socks5://user:pass@host:port&#10;http://user:pass@host:port"
            />
            <p
              v-if="invalidStaticLines.length"
              class="mt-1.5 text-[11px] text-destructive"
            >
              第 {{ invalidStaticLines.join('、') }} 行格式不对：需以 http://、https:// 或 socks5:// 开头
            </p>
          </div>
          <div>
            <div class="mb-1.5 flex items-baseline justify-between">
              <p class="text-sm font-medium text-foreground">
                轮换出口池（{{ rotatingProxyLines.length }} 条）
              </p>
              <p class="text-[11px] text-muted-foreground">
                一条 URL = 住宅网关，每次连接换地址
              </p>
            </div>
            <Textarea
              v-model="rotatingProxyText"
              rows="5"
              class="font-mono text-xs"
              placeholder="socks5h://user:pass@gw.example:7000"
            />
            <p
              v-if="invalidRotatingLines.length"
              class="mt-1.5 text-[11px] text-destructive"
            >
              第 {{ invalidRotatingLines.join('、') }} 行格式不对：需以 http://、https:// 或 socks5:// 开头
            </p>
          </div>
          <p class="text-[11px] leading-5 text-muted-foreground lg:col-span-2">
            格式 <code class="rounded bg-muted px-1">scheme://用户:密码@主机:端口</code>，scheme ∈ socks5 / socks5h / http / https，必须带 scheme。
            静态池优先——静态额度会过期，先花会过期的那份。
          </p>
        </div>
      </CardSection>

      <!-- 代理连通性检查 -->
      <CardSection
        title="代理连通性检查"
        description="逐条测已保存的代理能否到达上游：不带凭据、不消耗额度。401 = 通、403 = 出口被拒、429 = 被限速、连不上 = 代理本身坏了。"
      >
        <template #actions>
          <Button
            variant="outline"
            size="sm"
            :disabled="checkingProxies"
            @click="runProxyCheck"
          >
            <Network
              class="mr-2 h-4 w-4"
              :class="{ 'animate-pulse': checkingProxies }"
            />
            {{ checkingProxies ? '测试中...' : '测试连通性' }}
          </Button>
        </template>

        <div
          v-if="proxyCheckResults.length"
          class="overflow-x-auto rounded-xl border border-border"
        >
          <table class="min-w-[560px] w-full text-sm">
            <thead class="bg-muted/50 text-left text-xs font-medium text-muted-foreground">
              <tr>
                <th class="px-3 py-2.5">
                  出口
                </th>
                <th class="w-[70px] px-3 py-2.5">
                  池子
                </th>
                <th class="w-[130px] px-3 py-2.5">
                  结果
                </th>
                <th class="px-3 py-2.5">
                  出口地址
                </th>
              </tr>
            </thead>
            <tbody>
              <tr
                v-for="item in proxyCheckResults"
                :key="`${item.pool}:${item.proxy}`"
                class="border-t border-border align-top"
              >
                <td
                  class="max-w-[220px] truncate px-3 py-2.5 font-mono text-[11px]"
                  :title="item.proxy"
                >
                  {{ item.proxy }}
                  <p
                    v-if="item.warning"
                    class="mt-0.5 whitespace-normal text-[11px] text-amber-600 dark:text-amber-400"
                  >
                    ⚠ {{ item.warning }}
                  </p>
                </td>
                <td class="px-3 py-2.5">
                  <Badge
                    variant="outline"
                    class="text-[10px]"
                  >
                    {{ item.pool === 'static' ? '静态' : '轮换' }}
                  </Badge>
                </td>
                <td class="px-3 py-2.5">
                  <span
                    class="text-xs font-medium"
                    :class="proxyCheckResultClass(item)"
                  >{{ proxyCheckResultText(item) }}</span>
                </td>
                <td class="px-3 py-2.5 font-mono text-[11px] text-muted-foreground">
                  <template v-if="item.exit_ip">
                    {{ item.exit_ip }} · {{ item.country }} · {{ item.cf_colo }}
                  </template>
                  <template v-else>
                    -
                  </template>
                </td>
              </tr>
            </tbody>
          </table>
        </div>
        <p
          v-else
          class="rounded-xl border border-dashed border-border px-4 py-8 text-center text-sm text-muted-foreground"
        >
          点右上角「测试连通性」逐条核对已保存的代理，顺带核对有没有把轮换代理放进静态池。
        </p>
      </CardSection>

      <!-- 注入策略 -->
      <CardSection
        title="注入策略"
        description="决定业务请求发往上游前什么时候装桶里的正常态。"
      >
        <div class="space-y-4">
          <div class="grid grid-cols-1 gap-3 md:grid-cols-2">
            <button
              v-for="option in injectModeOptions"
              :key="option.value"
              type="button"
              class="rounded-xl border p-4 text-left transition-all duration-200"
              :class="config.inject_mode === option.value
                ? 'border-primary bg-primary/10 text-primary shadow-sm'
                : 'border-border bg-card/70 text-muted-foreground hover:border-primary/50 hover:text-foreground'"
              @click="config.inject_mode = option.value"
            >
              <span class="text-sm font-semibold">{{ option.label }}</span>
              <p class="mt-2 text-xs leading-relaxed text-muted-foreground">
                {{ option.helper }}
              </p>
            </button>
          </div>
          <div class="flex items-center justify-between rounded-xl border border-border bg-muted/40 px-4 py-3">
            <div>
              <p class="text-sm font-medium text-foreground">
                从业务流量被动采集
              </p>
              <p class="mt-0.5 text-[11px] text-muted-foreground">
                业务响应里带回的正常态直接入库，默认开；关掉后桶只能靠主动探测补。
              </p>
            </div>
            <Switch
              :model-value="config.harvest_inband"
              @update:model-value="(value: boolean) => config.harvest_inband = value"
            />
          </div>
        </div>
      </CardSection>

      <!-- 降智处置策略 -->
      <CardSection
        title="降智处置策略"
        description="连续 N 轮所有出口均采到降级态（312）才判定账号级降智；判定后对该账号的处置方式。采到 292 会自动恢复，不需要人工干预。"
      >
        <div class="space-y-4">
          <div class="grid grid-cols-1 gap-3 md:grid-cols-3">
            <button
              v-for="option in degradeActionOptions"
              :key="option.value"
              type="button"
              class="rounded-xl border p-4 text-left transition-all duration-200"
              :class="config.degrade_action === option.value
                ? 'border-primary bg-primary/10 text-primary shadow-sm'
                : 'border-border bg-card/70 text-muted-foreground hover:border-primary/50 hover:text-foreground'"
              @click="config.degrade_action = option.value"
            >
              <span class="text-sm font-semibold">{{ option.label }}</span>
              <p class="mt-2 text-xs leading-relaxed text-muted-foreground">
                {{ option.helper }}
              </p>
            </button>
          </div>
          <div class="flex items-center gap-3 rounded-xl border border-border bg-muted/40 px-4 py-3">
            <p class="shrink-0 text-sm font-medium text-foreground">
              判定阈值
            </p>
            <Input
              :model-value="config.degrade_threshold"
              type="number"
              min="1"
              max="20"
              size="sm"
              class="w-24 text-center tabular-nums"
              @update:model-value="(value) => config.degrade_threshold = clampInt(value, 1, 20, 3)"
            />
            <p class="text-[11px] text-muted-foreground">
              连续 N 轮全出口 312 才判降智，默认 3；调大更保守，调小更敏感。
            </p>
          </div>
        </div>
      </CardSection>

      <!-- 高级参数（折叠） -->
      <Collapsible v-model:open="advancedOpen">
        <section class="rounded-2xl border border-border bg-card">
          <CollapsibleTrigger as-child>
            <button
              type="button"
              class="flex w-full items-center justify-between p-5 text-left"
            >
              <span>
                <span class="text-sm font-semibold text-foreground">高级参数</span>
                <span class="mt-1 block text-xs text-muted-foreground">
                  TTL、长度指纹、续期与冷却。改长度指纹或 TTL 会清空全部已采桶。
                </span>
              </span>
              <ChevronDown
                class="h-4 w-4 shrink-0 text-muted-foreground transition-transform duration-200"
                :class="{ 'rotate-180': advancedOpen }"
              />
            </button>
          </CollapsibleTrigger>
          <CollapsibleContent>
            <div class="grid gap-x-8 gap-y-5 border-t border-border p-5 md:grid-cols-2">
              <div
                v-for="field in advancedFields"
                :key="field.key"
                class="space-y-1.5"
              >
                <div class="flex items-baseline justify-between gap-2">
                  <p class="font-mono text-xs font-medium text-foreground">
                    {{ field.key }}
                  </p>
                  <p
                    v-if="field.danger"
                    class="flex items-center gap-1 text-[11px] text-amber-600 dark:text-amber-400"
                  >
                    <AlertTriangle class="h-3 w-3" />
                    改动会清桶
                  </p>
                </div>
                <Input
                  :model-value="config[field.key]"
                  type="number"
                  :min="field.min"
                  :max="field.max"
                  size="sm"
                  class="w-32 tabular-nums"
                  @update:model-value="(value) => config[field.key] = clampInt(value, field.min, field.max, field.fallback)"
                />
                <p class="text-[11px] leading-5 text-muted-foreground">
                  {{ field.helper }}
                </p>
              </div>
              <div class="flex items-center justify-between rounded-xl border border-border bg-muted/40 px-4 py-3">
                <div>
                  <p class="font-mono text-xs font-medium text-foreground">
                    auto_renew
                  </p>
                  <p class="mt-0.5 text-[11px] text-muted-foreground">
                    剩余 TTL 低于续期阈值时自动起探测补桶。
                  </p>
                </div>
                <Switch
                  :model-value="config.auto_renew"
                  @update:model-value="(value: boolean) => config.auto_renew = value"
                />
              </div>
            </div>
          </CollapsibleContent>
        </section>
      </Collapsible>

      <!-- 规则说明 -->
      <section class="rounded-2xl border border-border bg-muted/30 p-5">
        <p class="mb-2 text-sm font-semibold text-foreground">
          四条硬规则
        </p>
        <ol class="list-decimal space-y-1 pl-5 text-sm text-muted-foreground">
          <li>绝不跨账号复用；绝不跨模型复用（同一账号内也不行）。</li>
          <li>同账号同模型可以跨 IP——采集换出口是为了绕过按 IP 的降级，使用时不需要跟着换。</li>
          <li>有效期从令牌自带的签发时间戳起算（默认 1 小时），过期上游直接拒绝，所以续期不能停。</li>
          <li>模块从不伪造值，只复用真实上游响应里采到的正常态；值永不进日志、永不下发前端。</li>
        </ol>
      </section>
    </div>
  </PageContainer>
</template>

<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { AlertTriangle, ChevronDown, Download, Network, Play, Plus, Recycle, RefreshCw, Square, Trash2 } from 'lucide-vue-next'
import { PageContainer, PageHeader, CardSection } from '@/components/layout'
import Badge from '@/components/ui/badge.vue'
import Button from '@/components/ui/button.vue'
import Checkbox from '@/components/ui/checkbox.vue'
import Collapsible from '@/components/ui/collapsible.vue'
import CollapsibleContent from '@/components/ui/collapsible-content.vue'
import CollapsibleTrigger from '@/components/ui/collapsible-trigger.vue'
import Input from '@/components/ui/input.vue'
import Pagination from '@/components/ui/pagination.vue'
import Switch from '@/components/ui/switch.vue'
import Textarea from '@/components/ui/textarea.vue'
import {
  codexTurnStateApi,
  type TurnStateAccountVerdict,
  type TurnStateBucket,
  type TurnStateConfig,
  type TurnStateDegradeAction,
  type TurnStateInjectMode,
  type TurnStateProbeRun,
  type TurnStateProxyCheckItem,
  type TurnStateScope,
  type TurnStateStatus,
} from '@/api/codex-turn-state'
import { getPoolOverview, listPoolKeys } from '@/api/endpoints/pool'
import { useModuleStore } from '@/stores/modules'
import { useConfirm } from '@/composables/useConfirm'
import { useToast } from '@/composables/useToast'
import { parseApiError } from '@/utils/errorParser'
import { log } from '@/utils/logger'

const DEFAULT_CONFIG: TurnStateConfig = {
  inject_mode: 'replace-only',
  harvest_inband: true,
  degrade_action: 'downweight',
  degrade_threshold: 3,
  ttl_seconds: 3600,
  template_length: 292,
  replace_length: 312,
  auto_renew: true,
  renew_threshold_seconds: 300,
  account_backoff_seconds: 600,
  exit_cooldown_seconds: 3300,
  rotating_max_attempts: 10,
  max_accounts_in_flight: 4,
  rotating_cooldown_seconds: 600,
  network_cooldown_seconds: 300,
  probe_account_pace_seconds: 2,
  proxy_check_timeout_seconds: 8,
  proxy_check_concurrency: 6,
  proxy_check_total_budget_seconds: 45,
}

const injectModeOptions: Array<{ value: TurnStateInjectMode; label: string; helper: string }> = [
  { value: 'replace-only', label: 'replace-only（默认）', helper: '仅当请求自带降级态（312）时才用桶里的 292 替换，不带头的请求原样放行。' },
  { value: 'always', label: 'always', helper: '只要桶里有未过期的正常态就装头，请求带不带头都装。覆盖面最大，写流量也最激进。' },
]

const degradeActionOptions: Array<{ value: TurnStateDegradeAction; label: string; helper: string }> = [
  { value: 'none', label: 'none', helper: '只换头不动账号。账号级降智只记日志和计数，调度行为不变。' },
  { value: 'downweight', label: 'downweight（默认）', helper: '写入健康分降权，让调度暂时少用这个账号，恢复后自动回升。' },
  { value: 'disable', label: 'disable', helper: '直接冷却禁用该账号，直到重新采到 292 自动解除。最狠，误伤面也最大。' },
]

interface AdvancedField {
  key: keyof TurnStateConfig & string
  min: number
  max: number
  fallback: number
  helper: string
  danger?: boolean
}

const advancedFields: AdvancedField[] = [
  { key: 'ttl_seconds', min: 300, max: 86400, fallback: 3600, danger: true, helper: '桶有效期（秒），从令牌自带签发时间戳起算，默认 3600。' },
  { key: 'template_length', min: 1, max: 65535, fallback: 292, danger: true, helper: '正常态头长度指纹，默认 292。上游格式变了才需要改。' },
  { key: 'replace_length', min: 1, max: 65535, fallback: 312, danger: true, helper: '降级态头长度指纹，默认 312。识别降级靠它，别乱动。' },
  { key: 'renew_threshold_seconds', min: 60, max: 3600, fallback: 300, helper: '剩余 TTL 低于该值（秒）触发自动续期，默认 300。' },
  { key: 'account_backoff_seconds', min: 60, max: 86400, fallback: 600, helper: '探测失败后同一账号的退避时间（秒），默认 600。' },
  { key: 'exit_cooldown_seconds', min: 300, max: 86400, fallback: 3300, helper: '单条静态出口对同一桶的冷却（秒），默认 3300（55 分钟）。' },
  { key: 'rotating_max_attempts', min: 1, max: 100, fallback: 10, helper: '轮换池对同一桶最多连试次数，默认 10。' },
  { key: 'max_accounts_in_flight', min: 1, max: 32, fallback: 4, helper: '同时在探测的账号数上限，默认 4。' },
  { key: 'rotating_cooldown_seconds', min: 60, max: 86400, fallback: 600, helper: '轮换池整池冷却（秒），默认 600（10 分钟）。一轮探测把池子转完后歇这么久再转。' },
  { key: 'network_cooldown_seconds', min: 30, max: 3600, fallback: 300, helper: '出口网络错误的短冷却（秒），默认 300。网络抖动不当作降智，休息一会儿再试。' },
  { key: 'probe_account_pace_seconds', min: 1, max: 60, fallback: 2, helper: '同一账号两次上游探测的最小间隔（秒），默认 2。防止把账号打出风控。' },
  { key: 'proxy_check_timeout_seconds', min: 2, max: 60, fallback: 8, helper: '代理可用性检查的单次请求超时（秒），默认 8。' },
  { key: 'proxy_check_concurrency', min: 1, max: 16, fallback: 6, helper: '代理检查的并发数，默认 6。代理多时可调大加快检查。' },
  { key: 'proxy_check_total_budget_seconds', min: 10, max: 300, fallback: 45, helper: '代理检查的总时间预算（秒），默认 45。超预算的剩余条目标记为预算耗尽。' },
]

const moduleStore = useModuleStore()
const { success, error } = useToast()
const { confirmDanger, confirmWarning } = useConfirm()

const loading = ref(false)
const saving = ref(false)
const status = ref<TurnStateStatus | null>(null)
const config = ref<TurnStateConfig>({ ...DEFAULT_CONFIG })
const originalConfigJson = ref('')
const scope = ref<TurnStateScope>({ key_ids: [], models: [], probe_proxies: [], probe_proxies_rotating: [] })
const originalScopeJson = ref('')
const staticProxyText = ref('')
const rotatingProxyText = ref('')
const newModelInput = ref('')
const proxyCheckResults = ref<TurnStateProxyCheckItem[]>([])
const checkingProxies = ref(false)
const clearing = ref(false)
const dryRunSaving = ref(false)
const moduleToggling = ref(false)
const scopeKeyPage = ref(1)
const scopeKeyPageSize = ref(10)
const accountSaving = ref(false)
const proxySaving = ref(false)
const importingKeys = ref(false)
const keysImported = ref(false)
const importedKeys = ref<Array<{ key_id: string; key_name: string; provider_name: string | null }>>([])
const probeActionPending = ref(false)
const advancedOpen = ref(false)

let probePollTimer: ReturnType<typeof setInterval> | null = null
let ttlTicker: ReturnType<typeof setInterval> | null = null

// TTL 本地倒计时：以最近一次拿到 status 的时刻为锚点，每秒递减显示值
const tickNow = ref(Date.now())
const statusLoadedAtMs = ref(Date.now())

const buckets = computed(() => status.value?.buckets ?? [])
const readyBuckets = computed(() => buckets.value.filter(bucket => bucket.ready))

// 桶矩阵分页：就绪桶排前，其余按 账号+模型 字典序，保证翻页稳定
const bucketPage = ref(1)
const bucketPageSize = ref(10)
const sortedBuckets = computed(() => [...buckets.value].sort((a, b) => {
  if (a.ready !== b.ready) return a.ready ? -1 : 1
  return `${a.key_id}:${a.model}`.localeCompare(`${b.key_id}:${b.model}`)
}))
const pagedBuckets = computed(() => {
  const start = (bucketPage.value - 1) * bucketPageSize.value
  return sortedBuckets.value.slice(start, start + bucketPageSize.value)
})
watch(() => sortedBuckets.value.length, (total) => {
  const maxPage = Math.max(1, Math.ceil(total / bucketPageSize.value))
  if (bucketPage.value > maxPage) bucketPage.value = maxPage
})
const probeRun = computed<TurnStateProbeRun | null>(() => status.value?.probe_run ?? null)
const accounts = computed(() => status.value?.accounts ?? [])

// 账号状态分页：与探测队列同序（降智 > 疑似 > 正常），同级按账号名字典序，翻页稳定
const accountPage = ref(1)
const accountPageSize = ref(10)
const verdictRank: Record<TurnStateAccountVerdict, number> = { degraded: 0, suspected: 1, normal: 2 }
const sortedAccounts = computed(() => [...accounts.value].sort((a, b) => {
  if (verdictRank[a.verdict] !== verdictRank[b.verdict]) return verdictRank[a.verdict] - verdictRank[b.verdict]
  return a.key_name.localeCompare(b.key_name) || a.key_id.localeCompare(b.key_id)
}))
const pagedAccounts = computed(() => {
  const start = (accountPage.value - 1) * accountPageSize.value
  return sortedAccounts.value.slice(start, start + accountPageSize.value)
})
watch(() => sortedAccounts.value.length, (total) => {
  const maxPage = Math.max(1, Math.ceil(total / accountPageSize.value))
  if (accountPage.value > maxPage) accountPage.value = maxPage
})

const verdictMeta: Record<TurnStateAccountVerdict, { label: string; dotClass: string; textClass: string }> = {
  normal: { label: '正常', dotClass: 'bg-emerald-500', textClass: 'text-emerald-600 dark:text-emerald-400' },
  suspected: { label: '疑似降智', dotClass: 'bg-amber-500', textClass: 'text-amber-600 dark:text-amber-400' },
  degraded: { label: '降智中', dotClass: 'bg-destructive', textClass: 'text-destructive' },
}

const degradeActionLabels: Record<TurnStateDegradeAction, string> = {
  none: '只换头',
  downweight: '降权',
  disable: '禁用',
}

/** 进度条满格对应的总时长，跟随配置里的 ttl_seconds */
const ttlTotalSeconds = computed(() => config.value.ttl_seconds || DEFAULT_CONFIG.ttl_seconds)

const counterItems = computed(() => {
  const counters = status.value?.counters
  return [
    { label: 'harvest 入库', value: counters?.harvest ?? 0 },
    { label: 'substitute 替换', value: counters?.substitute ?? 0 },
    { label: 'inject 装头', value: counters?.inject ?? 0 },
    { label: 'pass 放行', value: counters?.pass ?? 0 },
    { label: 'skip 跳过', value: counters?.skip ?? 0 },
  ]
})

/** 可选账号 = 导入的 codex 号池账号 ∪ 已保存范围（范围里没被导入命中的 = 号池删除重建后的残余） */
const availableKeys = computed(() => {
  const map = new Map<string, { key_id: string; key_name: string; provider_name: string | null; missing: boolean }>()
  for (const key of importedKeys.value) {
    map.set(key.key_id, { key_id: key.key_id, key_name: key.key_name, provider_name: key.provider_name, missing: false })
  }
  for (const keyId of scope.value.key_ids) {
    if (!map.has(keyId)) {
      // 导入完成前无法区分"号池已删除"，只有导入过才能标残余
      map.set(keyId, { key_id: keyId, key_name: keyId, provider_name: null, missing: keysImported.value })
    }
  }
  return [...map.values()].sort((a, b) =>
    Number(b.missing) - Number(a.missing)
    || a.key_name.localeCompare(b.key_name)
    || a.key_id.localeCompare(b.key_id))
})

/** 导入了多个 codex 供应商时才显示账号来源，单池不吵 */
const showKeyProviderName = computed(() =>
  new Set(importedKeys.value.map(key => key.provider_name)).size > 1)

const pagedScopeKeys = computed(() => {
  const start = (scopeKeyPage.value - 1) * scopeKeyPageSize.value
  return availableKeys.value.slice(start, start + scopeKeyPageSize.value)
})
watch(() => availableKeys.value.length, (total) => {
  const maxPage = Math.max(1, Math.ceil(total / scopeKeyPageSize.value))
  if (scopeKeyPage.value > maxPage) scopeKeyPage.value = maxPage
})

/** 可选模型 = 桶矩阵 ∪ 已保存范围 */
const availableModels = computed(() => {
  const models = new Set<string>()
  for (const bucket of buckets.value) models.add(bucket.model)
  for (const model of scope.value.models) models.add(model)
  return [...models].sort()
})

const staticProxyLines = computed(() => staticProxyText.value.split('\n').map(line => line.trim()).filter(Boolean))
const rotatingProxyLines = computed(() => rotatingProxyText.value.split('\n').map(line => line.trim()).filter(Boolean))

const PROXY_URL_PATTERN = /^(https?|socks5h?):\/\/\S+:\d+$/i

function invalidProxyLineNumbers(text: string): number[] {
  const invalid: number[] = []
  text.split('\n').forEach((line, index) => {
    const trimmed = line.trim()
    if (trimmed && !PROXY_URL_PATTERN.test(trimmed)) invalid.push(index + 1)
  })
  return invalid
}

const invalidStaticLines = computed(() => invalidProxyLineNumbers(staticProxyText.value))
const invalidRotatingLines = computed(() => invalidProxyLineNumbers(rotatingProxyText.value))

const originalScope = computed<TurnStateScope | null>(() => {
  if (!originalScopeJson.value) return null
  try {
    return JSON.parse(originalScopeJson.value) as TurnStateScope
  } catch {
    return null
  }
})

function sameStringList(left: string[], right: string[]): boolean {
  return JSON.stringify([...left].sort()) === JSON.stringify([...right].sort())
}

/** 账号与模型卡的脏检查：只看 key_ids / models */
const accountCardDirty = computed(() => {
  const original = originalScope.value
  if (!original) return false
  return !sameStringList(scope.value.key_ids, original.key_ids ?? [])
    || !sameStringList(scope.value.models, original.models ?? [])
})

/** 代理配置卡的脏检查：只看两个出口池 */
const proxyCardDirty = computed(() => {
  const original = originalScope.value
  if (!original) return false
  return !sameStringList(staticProxyLines.value, original.probe_proxies ?? [])
    || !sameStringList(rotatingProxyLines.value, original.probe_proxies_rotating ?? [])
})

const hasConfigChanges = computed(() => {
  if (!originalConfigJson.value) return false
  return JSON.stringify(config.value) !== originalConfigJson.value
})

function currentScopePayload(): TurnStateScope {
  return {
    key_ids: [...scope.value.key_ids],
    models: [...scope.value.models],
    probe_proxies: staticProxyLines.value,
    probe_proxies_rotating: rotatingProxyLines.value,
  }
}

function toggleScopeKey(keyId: string, checked: boolean) {
  scope.value.key_ids = checked
    ? [...new Set([...scope.value.key_ids, keyId])]
    : scope.value.key_ids.filter(id => id !== keyId)
}

function toggleScopeModel(model: string, checked: boolean) {
  scope.value.models = checked
    ? [...new Set([...scope.value.models, model])]
    : scope.value.models.filter(item => item !== model)
}

function addCustomModel() {
  const model = newModelInput.value.trim()
  if (!model) return
  if (!/^[a-z0-9][a-z0-9.-]*$/i.test(model) || !model.includes('-')) {
    error('模型名必须是带横线的上游官方名，如 gpt-5.5-codex')
    return
  }
  if (!scope.value.models.includes(model)) {
    scope.value.models = [...scope.value.models, model]
  }
  newModelInput.value = ''
}

function clampInt(value: string | number, min: number, max: number, fallback: number): number {
  const parsed = typeof value === 'number' ? value : parseInt(value, 10)
  if (Number.isNaN(parsed)) return fallback
  return Math.min(max, Math.max(min, parsed))
}

function displayTtl(bucket: TurnStateBucket): number | null {
  if (bucket.ttl_remaining_seconds == null) return null
  const elapsed = Math.floor((tickNow.value - statusLoadedAtMs.value) / 1000)
  return Math.max(0, bucket.ttl_remaining_seconds - elapsed)
}

/** TTL 显示为 分:秒 */
function formatTtl(seconds: number): string {
  const minutes = Math.floor(seconds / 60)
  const rest = seconds % 60
  return `${minutes}:${String(rest).padStart(2, '0')}`
}

function ttlBarClass(seconds: number): string {
  if (seconds <= 300) return 'bg-destructive'
  if (seconds <= 600) return 'bg-amber-500'
  return 'bg-emerald-500'
}

function ttlTextClass(seconds: number): string {
  if (seconds <= 300) return 'text-destructive'
  if (seconds <= 600) return 'text-amber-600 dark:text-amber-400'
  return 'text-muted-foreground'
}

function formatTime(unix: number): string {
  return new Date(unix * 1000).toLocaleString('zh-CN', { hour12: false })
}

function proxyCheckResultText(item: TurnStateProxyCheckItem): string {
  if (!item.reachable) return item.detail || '连不上'
  if (item.status_code === 401) return '通 (401)'
  return item.detail || `通 (${item.status_code})`
}

function proxyCheckResultClass(item: TurnStateProxyCheckItem): string {
  if (!item.reachable || item.status_code == null) return 'text-muted-foreground'
  if (item.status_code === 401) return 'text-emerald-600 dark:text-emerald-400'
  if (item.status_code === 403) return 'text-destructive'
  if (item.status_code === 429) return 'text-amber-600 dark:text-amber-400'
  return 'text-foreground'
}

async function loadStatus() {
  status.value = await codexTurnStateApi.getStatus()
  statusLoadedAtMs.value = Date.now()
}

async function loadAll() {
  loading.value = true
  try {
    const [statusData, scopeData, configData] = await Promise.all([
      codexTurnStateApi.getStatus(),
      codexTurnStateApi.getScope(),
      codexTurnStateApi.getConfig(),
      moduleStore.fetchModules(),
    ])
    status.value = statusData
    statusLoadedAtMs.value = Date.now()
    scope.value = { ...scopeData }
    staticProxyText.value = scopeData.probe_proxies.join('\n')
    rotatingProxyText.value = scopeData.probe_proxies_rotating.join('\n')
    originalScopeJson.value = JSON.stringify(scopeData)
    config.value = { ...configData }
    originalConfigJson.value = JSON.stringify(configData)
    syncProbePolling()
  } catch (err) {
    error(parseApiError(err, '加载模块状态失败'))
    log.error('Failed to load codex turn-state status', err)
  } finally {
    loading.value = false
  }
}

function applySavedScope(saved: TurnStateScope) {
  scope.value = { ...saved }
  staticProxyText.value = saved.probe_proxies.join('\n')
  rotatingProxyText.value = saved.probe_proxies_rotating.join('\n')
  originalScopeJson.value = JSON.stringify(saved)
}

/** 导入 codex 号池账号：号池概览过滤 provider_type=codex，逐供应商拉全量 key */
async function importPoolKeys(options: { silent?: boolean } = {}) {
  importingKeys.value = true
  try {
    const overview = await getPoolOverview()
    const codexProviders = overview.items.filter(item => item.provider_type === 'codex')
    const collected: Array<{ key_id: string; key_name: string; provider_name: string | null }> = []
    for (const provider of codexProviders) {
      let page = 1
      const pageSize = 100
      let fetched = 0
      // 页数上限 50（5000 条）兜底，防接口异常时死循环
      while (page <= 50) {
        const resp = await listPoolKeys(provider.provider_id, { page, page_size: pageSize })
        for (const key of resp.keys) {
          collected.push({ key_id: key.key_id, key_name: key.key_name, provider_name: provider.provider_name })
        }
        fetched += resp.keys.length
        if (fetched >= resp.total || resp.keys.length < pageSize) break
        page += 1
      }
    }
    const dedup = new Map<string, { key_id: string; key_name: string; provider_name: string | null }>()
    for (const item of collected) {
      if (!dedup.has(item.key_id)) dedup.set(item.key_id, item)
    }
    importedKeys.value = [...dedup.values()]
    keysImported.value = true
    if (!options.silent) success(`已导入 ${importedKeys.value.length} 个 codex 号池账号`)
  } catch (err) {
    error(parseApiError(err, '导入号池账号失败'))
  } finally {
    importingKeys.value = false
  }
}

async function saveAccountScope() {
  accountSaving.value = true
  try {
    const saved = await codexTurnStateApi.updateScope(currentScopePayload())
    applySavedScope(saved)
    success('账号与模型已保存')
  } catch (err) {
    error(parseApiError(err, '保存账号与模型失败'))
  } finally {
    accountSaving.value = false
  }
}

async function saveProxyScope() {
  if (invalidStaticLines.value.length || invalidRotatingLines.value.length) {
    error('代理池里有格式不对的行，先修正再保存')
    return
  }
  proxySaving.value = true
  try {
    const saved = await codexTurnStateApi.updateScope(currentScopePayload())
    applySavedScope(saved)
    success('代理配置已保存')
  } catch (err) {
    error(parseApiError(err, '保存代理配置失败'))
  } finally {
    proxySaving.value = false
  }
}

async function saveConfig() {
  const original: TurnStateConfig | null = originalConfigJson.value
    ? JSON.parse(originalConfigJson.value) as TurnStateConfig
    : null
  const fingerprintChanged = original != null && (
    original.ttl_seconds !== config.value.ttl_seconds ||
    original.template_length !== config.value.template_length ||
    original.replace_length !== config.value.replace_length
  )
  if (fingerprintChanged) {
    const ok = await confirmWarning(
      '修改 TTL 或长度指纹会使全部已采桶作废并立即清空，需要重新探测采集。确定要保存吗？',
      '改动会清空全部已采桶',
    )
    if (!ok) return
  }
  saving.value = true
  try {
    const saved = await codexTurnStateApi.updateConfig({ ...config.value })
    config.value = { ...saved }
    originalConfigJson.value = JSON.stringify(saved)
    if (fingerprintChanged) await loadStatus()
    success('配置已保存')
  } catch (err) {
    error(parseApiError(err, '保存配置失败'))
  } finally {
    saving.value = false
  }
}

async function toggleModuleEnabled(value: boolean) {
  moduleToggling.value = true
  try {
    await moduleStore.setEnabled('codex_turn_state', value)
    await loadStatus()
    success(value ? '模块已启用' : '模块已停用')
  } catch (err) {
    error(parseApiError(err, '切换模块启用状态失败'))
  } finally {
    moduleToggling.value = false
  }
}

async function toggleDryRun(value: boolean) {
  dryRunSaving.value = true
  try {
    const result = await codexTurnStateApi.setDryRun(value)
    if (status.value) status.value.dry_run = result.dry_run
    success(result.dry_run ? '已开启 Dry-Run' : '已关闭 Dry-Run')
  } catch (err) {
    error(parseApiError(err, '切换 Dry-Run 失败'))
  } finally {
    dryRunSaving.value = false
  }
}

async function startProbe() {
  probeActionPending.value = true
  try {
    await codexTurnStateApi.startProbe()
    await loadStatus()
    syncProbePolling()
    success('探测已启动')
  } catch (err) {
    error(parseApiError(err, '启动探测失败'))
  } finally {
    probeActionPending.value = false
  }
}

/** 账号行「立即探测」：定向巡检该账号的全部模型，降智账号采到 292 即自动恢复 */
async function startAccountProbe(keyId: string) {
  probeActionPending.value = true
  try {
    await codexTurnStateApi.startProbe([keyId])
    await loadStatus()
    syncProbePolling()
    success('已启动定向探测')
  } catch (err) {
    error(parseApiError(err, '启动定向探测失败'))
  } finally {
    probeActionPending.value = false
  }
}

async function cancelProbe() {
  probeActionPending.value = true
  try {
    await codexTurnStateApi.cancelProbe()
    await loadStatus()
    syncProbePolling()
  } catch (err) {
    error(parseApiError(err, '取消探测失败'))
  } finally {
    probeActionPending.value = false
  }
}

async function runProxyCheck() {
  checkingProxies.value = true
  try {
    proxyCheckResults.value = await codexTurnStateApi.proxyCheck()
  } catch (err) {
    error(parseApiError(err, '连通性测试失败'))
  } finally {
    checkingProxies.value = false
  }
}

async function clearBuckets() {
  const ok = await confirmDanger(
    '清空后所有桶回到空状态，下次业务请求由被动采集重新接管，续期要等下一轮探测。确定要清空吗？',
    '清空全部已采桶',
  )
  if (!ok) return
  clearing.value = true
  try {
    const result = await codexTurnStateApi.clearBuckets()
    await loadStatus()
    success(`已清空 ${result.cleared} 个桶，被动采集将重新接管`)
  } catch (err) {
    error(parseApiError(err, '清空桶失败'))
  } finally {
    clearing.value = false
  }
}

function syncProbePolling() {
  const running = status.value?.probe_run.running === true
  if (running && !probePollTimer) {
    probePollTimer = setInterval(async () => {
      try {
        await loadStatus()
        if (!status.value?.probe_run.running) syncProbePolling()
      } catch {
        // 轮询失败静默，下次继续
      }
    }, 2000)
  } else if (!running && probePollTimer) {
    clearInterval(probePollTimer)
    probePollTimer = null
  }
}

onMounted(() => {
  loadAll()
  // 静默预导入一次：让已保存范围的账号名直接可见，号池删除重建后的残余 key 立刻可标
  importPoolKeys({ silent: true })
  ttlTicker = setInterval(() => {
    tickNow.value = Date.now()
  }, 1000)
})

onBeforeUnmount(() => {
  if (probePollTimer) clearInterval(probePollTimer)
  if (ttlTicker) clearInterval(ttlTicker)
})
</script>
