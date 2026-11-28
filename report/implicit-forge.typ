#import "@preview/lilaq:0.6.0" as lq
#import "@preview/zero:0.7.1": num, set-num, set-unit, zi

#let title = "ImplicitForge · 交互式距离场建模与渲染"
#let author = "arshtyi"
#let name = ("彭靖轩", "崔家瑞")
#let date = datetime.today()

#set document(title: title, author: author, date: date)
#set page(paper: "a4", margin: (x: 19mm, top: 16mm, bottom: 17mm), numbering: "1 / 1")
#set text(lang: "zh", region: "cn", font: ((name: "Lato", covers: "latin-in-cjk"), "Noto Serif CJK SC"), size: 11pt)
#set par(justify: true)
#let ink = rgb("#174960")
#let amber = rgb("#be733d")
#show heading: set text(font: ((name: "Lato", covers: "latin-in-cjk"), "Noto Sans CJK SC"), fill: ink)
#set heading(numbering: "1.1.1.")
#show figure.caption: set text(size: 9.5pt)
#set table(stroke: none, inset: (x: 5pt, y: 5pt), align: (col, row) => if col == 0 { left } else { right })
#show raw: set text(font: "JetBrains Mono")
#show raw.where(block: false): box.with(
    fill: luma(240),
    inset: (x: .3em, y: 0em),
    outset: (x: 0em, y: .3em),
    radius: .2em,
)
#set-unit(fraction: "power")

#let (ms,) = (
    zi.declare("ms"),
)
#let rule = table.hline(stroke: 0.7pt + ink)
#let thin = table.hline(stroke: 0.4pt + luma(175))
#let frames = csv("data/frames.csv", row-type: dictionary)
#let run = json("data/run.json")
#let tree = csv(
    bytes(read("data/stamp-tree.csv").split("\n").filter(line => not line.starts-with("#")).join("\n")),
    row-type: dictionary,
)
#let window-run = json("data/window.json")
#let labels = (few: [少量], many: [密集], sparse: [稀疏], chain: [深链], sculpture: [雕塑])
#let fixtures = ("few", "many", "sparse", "chain")
#let cases = fixtures + ("sculpture",)
#let modes = ("serial-full", "serial-pruned", "rayon-full", "rayon-pruned")
#let group(id, mode, w: 160) = frames.filter(r => r.case == id and r.mode == mode and int(r.width) == w)
#let record(id, mode, w: 160) = group(id, mode, w: w).first()
#let percentile(id, mode, p: 50, w: 160) = {
    let xs = group(id, mode, w: w).map(r => float(r.ms)).sorted()
    xs.at(calc.ceil(xs.len() * p / 100) - 1)
}
#let dec(x, n: 2) = str(calc.round(x, digits: n))
#let delay(id, mode, w: 160) = dec(percentile(id, mode, w: w)) + " / " + dec(percentile(id, mode, p: 95, w: w))
#let count(id, mode) = int(record(id, mode).primitive_evals)
#let gpu = "wgpu-full"
#let is-compute = run.adapter != none
#let names = (
    [镂空雕塑],
    [联轴器],
    [悬挑建筑],
    [短波收音机],
    [港口灯塔],
    [机器人],
    [拍立得相机],
    [石拱高架桥],
    [茶杯与茶碟],
)
#let photo(path, caption, pixel: false) = stack(
    dir: ttb,
    spacing: 5pt,
    image("figures/" + path, width: 100%, scaling: if pixel { "pixelated" } else { "smooth" }, alt: caption),
    align(center, text(size: 8.5pt, caption)),
)

#text(size: 25pt, weight: "bold", fill: ink)[ImplicitForge]
#h(8pt)#text(size: 15pt)[交互式距离场建模与渲染]

#name.join(" · ")

// #outline()

= 目标与使用

隐式建模：几何、法线和阴影都取自同一个距离场，无需重建三角网格。

项目研究两个问题：每个追踪步能省掉多少图元查询，以及普通 CPU 能否在编辑时尽快交回一张预览。

#figure(
    grid(
        columns: (1fr,) * 3, gutter: 4pt,
        ..names.enumerate().map(((i, name)) => photo("demo-" + str(i + 1) + ".png", str(i + 1) + "  " + name.text))
    ),
    caption: [九个预设。],
)

== 演示

+ 运行 `just run`。按 `1`--`9` 选场景，拖动旋转，滚轮缩放，`R` 复位相机。窗口启动即显示 CPU 画面。
+ 选 `2` 打开联轴器。在 Stamp tool 中开启预览，选择 Cylinder、Axis Z 和 Subtract，设置半径、高度与中心。在同一位置改选 Smooth cut，调节 Blend width 可改变孔缘。Apply 保留这一层，Undo 撤销最近一层。
+ `B` 在 CPU 全树和剪枝之间切换，`G` 或面板按钮选择计算设备。观察同一轮廓、孔壁阴影和底部帧耗时。
+ 点击 Cost map 或按 `H` 进入 CPU 图元求值热力图。保持相机静止，在 Target 阶段按 `B` 比较全树与剪枝的查询量。

= 原理

== 图元与组合

设 $p$ 为世界坐标，$q=p-c$ 为相对图元中心的位置。场的符号描述点在内部、表面或外部（负、零、正）。球面只需一次长度计算。盒体的三个参数是半边长；对盒外点分别截取沿各轴越界的正分量，盒内点则保留到最近一面的负值。圆环绕 $y$ 轴，先在水平面求到圆心轴的径向距离，再求到管截面的距离。

#figure(
    table(
        columns: (1fr, 2fr),
        align: left,
        rule,
        table.header([*形状*], [*局部场值*]),
        thin, [球，半径 $r$],
        $norm(q)-r$, [盒，半边长 $b$],
        [$u=abs(q)-b$，$d=norm(max(u, 0))+min(max(u_x, u_y, u_z), 0)$], [圆环，主半径 $R$、管半径 $r$],
        [$rho=sqrt(q_x^2+q_z^2)$，$d=sqrt((rho-R)^2+q_y^2)-r$], [有限圆柱，半径 $r$、半高 $h$],
        [$u=(rho-r, abs(q_y)-h)$，$d=norm(max(u, 0))+min(max(u_1, u_2), 0)$], rule,
    ),
    caption: [盒体公式中的绝对值和与零的最大值逐分量计算。],
)

设左、右子树的场值为 $a,b$。并集取 $min(a, b)$，交集取 $max(a, b)$，差集取 $max(a, -b)$。因此“盒减球”留下凹洞，“球减盒”留下球的剩余部分。平滑并集在两支场值接近时降低接缝附近的值：

$ h=max(1-abs(a-b)/k, 0), quad d=min(a, b)-k h^2/4 quad (k>0). $

融合宽度 $k=0$ 时直接取硬并集。融合区内梯度是两侧梯度的凸组合；`min`、`max` 和变号也保持 1-Lipschitz 性。组合场的绝对值因而仍可用作到零等值面的保守步长。

圆柱、圆环和非等边盒可将局部 $y$ 轴对齐世界 $x$、$y$ 或 $z$ 轴。对这三类轴对称或镜面对称图元，交换坐标轴即可求精确距离，包围界的半边长也须随轴交换。盒的三个局部尺寸独立可调，圆柱独立控制半径和高度。记平滑并集为 $s(a,b,k)$，平滑减法为 $-s(-a,b,k)$，零宽度退化为 $max(a, -b)$，光滑过渡宽度与印章尺寸独立。

== 球面追踪与着色

相机向一个子像素发出单位方向 $v$。若 $s$ 是任一表面点，因 $d(s)=0$，有 $abs(d(p)) <= norm(p-s)$。以当前位置为中心、以 $abs(d(p))$ 为半径的开球内没有零点。反复令 $t_(i+1)=t_i+abs(d(o+t_i v))$；小于 $0.001$ 单位即命中。主射线至多 $128$ 步、最远 $32$ 单位，先到达解析地面时由地面接收阴影。

命中处沿三个坐标轴做六次中心差分，间距 $0.0015$，归一化得到法线。定向光提供漫反射和半程向量高光。从离开表面的一点向光源再追踪最多 $48$ 步，以沿途距离和已走路程的比值估计阴影软硬。

= 实现

== 数据流

JSON 场景定义有序二叉树；可调数值用参数索引和两个端点表示。滑块值 $t in [0,1]$ 在端点间线性插值。加载器检查参数索引、尺寸和端点，再按左子树、右子树、运算符的顺序产生后缀指令。如 `BOX SPHERE SUBTRACT TORUS UNION` 表示先在盒上挖球洞，再加圆环。

#figure(
    table(
        columns: (1fr, 4fr, 4fr),
        align: left,
        rule,
        table.header([*环节*], [*Rayon CPU*], [*wgpu / WGSL*]),
        thin, [几何查询], [由同一指令流构造带子节点索引的树。基线递归访问两支。],
        [迭代解释后缀指令。遇二元运算先弹右值、再弹左值。],
        [并发],
        [$16 times 16$ 分块直接写入像素缓冲；边缘块裁切，空闲线程窃取待算块。],

        [一个 $8 times 8$ 工作组覆盖一块像素，越界调用提前返回。],
        [结果交付],
        [同一分块算法也可串行执行，便于隔离并行收益。],

        [像素缓冲复制到可映射缓冲，等待设备完成后读回主存。], rule,
    ),
    caption: [最终由 softbuffer 呈现，GPU 无需承担窗口显示。],
)

每条指令是三个 $16$ 字节向量，共 $48$ 字节；相机与帧参数占 $96$ 字节。宿主在上传前检查操作码、栈下溢和最终栈深。解释器最多接收 $255$ 条指令和 $64$ 个操作数。

== 保守剪枝

对硬并集，先求一侧得到当前最小值 $m$。若另一侧在同一点的有符号下界为 $L$，则 $L>m+epsilon$ 才能跳过它。浮点裕量 $epsilon$ 随场景尺度和查询坐标放大。已知包围盒的每个外向支撑平面都给出下界：在盒外可能为正，在盒内仍为负。若错误地把盒内距离截成零，在重叠实体内部可能丢掉更负的一支。

例如，半径为 $1$ 和 $2$ 的同心球做并集。在球心先求小球得到 $m=-1$。若将大球的盒内距离截成 $0$，就会因为 $0>-1$ 而跳过它，错误地返回 $-1$。有符号支撑平面的下界为 $-2$，比较不能触发跳过，算法才会得到正确的 $-2$。

当两支均有界时，沿两盒中心分离最大的轴先查近侧，再用远侧支撑平面与 $m$ 比较。差与交的结果都不小于左操作数，可继承已证明的下界；运算内部仍求两支的真实值。正宽度融合最多额外降低 $k/4$，实现将其边界标为未知。单侧未知时先完整求该侧，两侧未知时全部求值。

== 重组连续并集

用户按顺序应用印章，连续硬并集满足 $min(min(a, b), c)=min(a, min(b, c))$。编译时将每个连续段按印章中心跨度最大的轴取中位数，递归构造空间上相邻的平衡子树，最后与之前的模型做并集。遇差、交或任一平滑操作即结束该段，保持原操作数顺序。撤销仍从输入列表末尾删除一枚印章。CPU 与 WGSL 共用重组后的后缀流。

= 编辑与图像质量

#figure(
    grid(
        columns: (1fr, 1fr),
        gutter: 6pt,
        photo("demo-1.png", "雕塑 · 初始"), photo("stamped.png", "同一相机 · 球体并集"),
    ),
    caption: [球印章的半径为 $0.6$，中心为 $(0, 0.25, 1)$。两幅图使用相同的材质、光源、分辨率和采样。],
)

初始雕塑的开口使观察者可以同时看到外缘、孔壁和内侧小球。在开口前加入球体后，$#run.figures.edit.changed$ 个像素发生变化，既包括球体遮挡的区域，也包括光源可见性变化造成的投影。差集可用同样的手势切开零件。

#figure(
    grid(
        columns: (1fr, 1fr),
        gutter: 6pt,
        photo("bore-hard.png", "有限圆柱 · 普通减法"), photo("bore-smooth.png", "同一刀具 · 平滑减法"),
    ),
    caption: [轴向 Z，中心 $(0,0.91,0)$，半径 $0.26$，全高 $2.4$。右图融合宽度 $0.20$。],
)

== 拖动与静止使用不同的采样预算

拖动时先渲染目标宽、高各四分之一的图像，每像素一条主射线。空闲 #ms[180] 后渲染半尺寸，再渲染目标尺寸。前两级按像素中心做双线性放大。目标帧在四个子像素位置各着色一次，在线性光空间平均，最后编码为 RGB8。在斜边上，前景与背景可以共同贡献一个像素的颜色。

#figure(
    grid(
        columns: (36mm, 36mm),
        gutter: 8pt,
        photo("aa-center.png", "像素中心单采样", pixel: true), photo("aa-2x2.png", "2 × 2 子像素采样", pixel: true),
    ),
    caption: [同一轮廓的原始像素，按最近邻放大。裁剪左上角为 $(#run.figures.crop.x, #run.figures.crop.y)$，边长$#run.figures.crop.side$ 像素，其中 $#run.figures.crop.changed$ 个像素随采样改变。],
)

每个请求带场景代次、输入版本和质量阶段。持续拖动时允许正在计算的预览先返回，再取最新姿态；新的精细帧只接受匹配版本。切换场景、后端或窗口尺寸会使旧任务失效。CPU 在块边界检查取消，已提交的 GPU 任务通过版本检查丢弃过期结果。常用窗口下目标帧与物理像素一一对应，较大的视口按比例限制在 $1440 times 1080$ 内。

= Benchmark

用 $8$ 个和 $32$ 个水平圆环检验数量影响。对 $32$ 个圆环，将中心间距从 $0.85$ 增至 $1.65$ 得到稀疏组；保持密集组几何不变，按输入顺序左结合得到深链组。两棵 $32$ 叶树的层数分别为 $6$ 和 $32$。第五组取默认雕塑，包含交、差与平滑并集。目标画质均为 $160 times 96$、每像素 $4$ 次采样。

每个变体预热一次，再轮换执行 $#run.samples$ 轮。计时从调用渲染器开始，到主存中得到可用像素为止，包含 GPU 提交、等待和读回。设备初始化、场景编译、误差统计与窗口合成在计时区间外。p50/p95 使用排序后的最近秩。求值与步数在独立插桩帧记录，常规计时关闭计数。

#figure(
    table(
        columns: (auto,) * 5,
        rule,
        table.header([*场景*], [*串行全树*], [*串行剪枝*], [*并行全树*], [*并行剪枝*]),
        thin,
        ..fixtures.map(id => (labels.at(id), ..modes.map(m => delay(id, m)))).flatten(), rule,
    ),
    caption: [固定画质下逐项改变查询与调度。每格为帧延迟 p50 / p95，单位 #ms()。],
)

#figure(
    lq.diagram(
        width: 100%,
        height: 5cm,
        ylabel: [图元求值 / 百万],
        xaxis: (ticks: fixtures.map(id => labels.at(id)).enumerate(), subticks: none),
        lq.bar(
            range(4),
            fixtures.map(id => count(id, "serial-full") / 1e6),
            width: 0.34,
            offset: -0.18,
            fill: ink,
            label: [全树],
        ),
        lq.bar(
            range(4),
            fixtures.map(id => count(id, "serial-pruned") / 1e6),
            width: 0.34,
            offset: 0.18,
            fill: amber,
            label: [剪枝],
        ),
    ),
    caption: [每帧球、盒、圆环的调用总数，包含主射线、阴影与法线探针。],
)

稀疏组省去 $#dec(100 * (1 - count("sparse", "serial-pruned") / count("sparse", "serial-full")), n: 1)%$ 的图元求值，主射线和阴影仍共走 $#record("sparse", "serial-full").ray_steps$ 步。串行剪枝相对串行全树的 p50 加速比为 $#dec(percentile("sparse", "serial-full") / percentile("sparse", "serial-pruned"))$ 倍。固定剪枝模式，改用 Rayon 后另得 $#dec(percentile("sparse", "serial-pruned") / percentile("sparse", "rayon-pruned"))$ 倍。少量组的剪枝耗时是全树的 $#dec(percentile("few", "serial-pruned") / percentile("few", "serial-full"))$ 倍。

== 实际印章序列与热力图

在默认雕塑上按行依次加入 $8 times 8$ 个球印章（半径 $0.33$，间距 $0.85$）。保持场景、相机、$160 times 96$ 四采样和 Rayon 剪枝一致，仅改变连续并集的结合顺序。两种布局分别预热，交替计时，计数在额外帧取得。以逐项添加的串行全树图像作共同参考。

#figure(
    table(
        columns: 5,
        rule,
        table.header([*印章布局*], [*p50 / p95（ms）*], [*图元求值*], [*追踪步数*], [*差异像素*]),
        thin,
        ..(("incremental", [按序左结合]), ("regrouped", [空间中位数]))
            .map(((key, label)) => {
                let rows = tree.filter(r => r.variant == key)
                let xs = rows.map(r => float(r.ms)).sorted()
                let q(p) = dec(xs.at(calc.ceil(xs.len() * p / 100) - 1))
                (
                    label,
                    q(50) + " / " + q(95),
                    rows.first().primitive_evals,
                    rows.first().ray_steps,
                    str(calc.max(..rows.map(r => int(r.differing_pixels)))),
                )
            })
            .flatten(),
        rule,
    ),
    caption: [印章树重组消融，计时范围为渲染与主存帧缓冲交付。],
)

#let calls(key) = int(tree.find(r => r.variant == key).primitive_evals)
本组的图元调用减少 $#dec(100 * (1 - calls("regrouped") / calls("incremental")), n: 1)%$。空间分组可整体跳过远端的一簇印章；短并集段或掺有其他操作的模型，其收益取决于查询点和子树的分布。表中渲染时间不含编辑后的重新编译开销。

#figure(
    grid(
        columns: (1fr, 1fr),
        gutter: 6pt,
        photo("heatmap-baseline.png", "CPU 全树"), photo("heatmap-pruned.png", "CPU 剪枝"),
    ),
    caption: [同视角的每像素图元调用次数。包含全部子像素的主射线、阴影和法线查询。固定色阶从冷到暖，下界依次为 $0$、$128$、$256$、$512$、$1024$、$2048$、$4096$、$8192$。],
)

= 后端对照与交互开销

#if is-compute {
    figure(
        table(
            columns: (1fr,) * 5,
            rule,
            table.header([*场景*], [*Rayon 全树*], [*WGSL 全树*], [*平均通道误差*], [*最大误差*]),
            thin,
            ..cases
                .map(id => (
                    labels.at(id),
                    delay(id, "rayon-full"),
                    delay(id, gpu),
                    dec(group(id, gpu).map(r => float(r.mae_rgb8)).sum() / run.samples, n: 5),
                    str(calc.max(..group(id, gpu).map(r => int(r.max_rgb8)))),
                ))
                .flatten(),
            rule,
        ),
        caption: [同一相机与四采样，含读回。p50 / p95 的单位为 #ms()；RGB 误差相对串行全树参考，通道范围 $0-255$。],
    )
} else [本轮只取得 CPU 数据，设备失败原因为 #run.unavailable。]

#let target-cpu = frames.filter(r => int(r.width) == 160 and r.mode != gpu)
本轮目标画质各 CPU 路径的最大通道误差为 $#calc.max(..target-cpu.map(r => int(r.max_rgb8)))$。

#if is-compute [以雕塑为例，Rayon 全树 p50 除以含读回的 WGSL p50 为 $#dec(percentile("sculpture", "rayon-full") / percentile("sculpture", gpu))$。]

#let sculpt = "sculpture"
#let pruned = "rayon-pruned"
#figure(
    table(
        columns: 4,
        rule,
        table.header([*雕塑质量阶段*], [*p50 / p95（ms）*], [*射线步数*], [*平均通道误差*]),
        thin,
        ..(40, 80, 160)
            .map(w => {
                let r = record(sculpt, pruned, w: w)
                (r.width + " × " + r.height, delay(sculpt, pruned, w: w), r.ray_steps, dec(float(r.mae_rgb8), n: 3))
            })
            .flatten(),
        rule,
    ),
    caption: [预览按界面相同的双线性插值放大后比较。误差与耗时共同描述画质取舍。],
)

雕塑预览的 p50 是目标帧的 $#dec(100 * percentile(sculpt, pruned, w: 40) / percentile(sculpt, pruned), n: 1)%$，代价是细孔和高光处误差增大。

== 真实窗口输入延迟

在有 XTEST 的 X11 会话中启动 release CPU 窗口，通过原生指针事件执行六次水平移动后释放。每轮从已稳定的 Target 开始，路径闭合以恢复初始姿态。应用在 winit 事件处理入口和 softbuffer 成功提交之后读取同一单调时钟，按渲染任务版本筛选帧。移动延迟从首次有效移动到第一张与当前姿态匹配的 Preview；释放延迟从松开鼠标到与最终姿态匹配的 Target。后者包含剩余静止等待、细化和呈现提交。

#if window-run.status == "complete" {
    let ready = window-run.ready
    [场景 #ready.scene，后端 #ready.backend，#ready.rayon_workers 个 Rayon 工作线程，窗口 #ready.window.map(str).join(" × ")，预览 #ready.preview.map(str).join(" × ")，目标 #ready.target.map(str).join(" × ")。预热 #window-run.warmup 轮，采集 #window-run.trials 轮，每步间隔约 #window-run.step_interval_ms ms，丢失移动事件 #window-run.measured_dropped_motions 个。]
    figure(
        table(
            columns: 4,
            rule,
            table.header([*测量区间*], [*有效轮次*], [*p50（ms）*], [*p95（ms）*]),
            thin,
            ..(("motion_preview_ms", [首次移动 → 匹配预览]), ("release_target_ms", [释放 → 匹配目标帧]))
                .map(((key, label)) => {
                    let s = window-run.summary.at(key)
                    (label, str(s.n), dec(s.p50), dec(s.p95))
                })
                .flatten(),
            rule,
        ),
        caption: [真实窗口实验，最近秩分位数。],
    )
} else [当前报告输入未包含完整窗口样本。#window-run.reason]

== 分工

- #name.at(0)：距离场、保守剪枝、CPU/GPU 渲染与同画质消融验证以及报告。
- #name.at(1)：预设与印章演示、相机和界面交互、效果对照图及现场项目展示。
