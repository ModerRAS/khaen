/** UI 层共享类型（徽标 / 真实性标签）。 */

/** 运行状态徽标种类。 */
export type StatusKind = 'running' | 'faulted' | 'muted' | 'safe' | 'unknown'

/** 数据真实性标签（PLAN §11.5：所有非实测数据必须带标签）。 */
export type TruthKind = 'actual' | 'demo' | 'notWired'
