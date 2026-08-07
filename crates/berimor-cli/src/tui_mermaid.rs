//! Mermaid-lite: разбор честного подмножества flowchart-диаграмм mermaid и
//! их отрисовка в текстовые строки (Unicode box-drawing) для TUI.
//! ROADMAP §20.26.
//!
//! Поддерживаемая грамматика:
//! - заголовок `graph TD` / `graph LR` (также `flowchart`, `TB` как синоним `TD`);
//! - объявления узлов `A[Label]`, `B(Label)`, `C{Label}`, метка в кавычках
//!   `A["Label с ] скобкой"]`;
//! - рёбра `A --> B`, `A -->|label| B`, цепочки `A --> B --> C`;
//! - «голые» узлы в рёбрах (неявное объявление, метка = идентификатор);
//! - разделители операторов — перевод строки и `;`, комментарии `%%`.
//!
//! Неизвестные конструкции — `Err` с указанием строки, без паник.
//! Раскладка намеренно простая (v1): BFS-слои + порядок объявления,
//! никакого Сугиямы. Циклы безопасны: множество посещённых узлов.

use std::collections::{HashMap, VecDeque};

/// Потолки размера графа: за пределами — честная ошибка, а не попытка
/// отрисовать «простыню» в чат.
const MAX_NODES: usize = 50;
const MAX_EDGES: usize = 100;

/// Высота бокса узла и вертикальный шаг слоя при TD-раскладке
/// (3 строки бокса + 2 строки стрелки).
const BOX_H: usize = 3;
const LAYER_STEP: usize = BOX_H + 2;

/// Направление диаграммы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Сверху вниз (`graph TD` / `TB`).
    TopDown,
    /// Слева направо (`graph LR`).
    LeftRight,
}

/// Форма узла: влияет только на угловые/боковые глифы бокса.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// `A[Label]` — прямоугольник.
    Rect,
    /// `A(Label)` — скруглённые углы.
    Round,
    /// `A{Label}` — ромб (условно: `╱─╲` и `< >` по бокам).
    Diamond,
}

#[derive(Debug, Clone)]
struct Node {
    #[allow(dead_code)] // id нужен парсеру/индексу; в рендере участвует метка
    id: String,
    label: String,
    shape: Shape,
}

#[derive(Debug, Clone)]
struct Edge {
    from: usize,
    to: usize,
    label: Option<String>,
}

/// Разобранная диаграмма.
#[derive(Debug, Clone)]
pub struct Graph {
    direction: Direction,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    index: HashMap<String, usize>,
}

impl Graph {
    /// Добавить узел по спецификации; повторное упоминание обновляет
    /// метку/форму, если они заданы явно. Возвращает индекс узла.
    fn add_node(&mut self, id: &str, label: Option<String>, shape: Shape) -> Result<usize, String> {
        if let Some(&idx) = self.index.get(id) {
            if let Some(l) = label {
                self.nodes[idx].label = l;
                self.nodes[idx].shape = shape;
            }
            return Ok(idx);
        }
        if self.nodes.len() >= MAX_NODES {
            return Err(format!(
                "слишком большая диаграмма: больше {MAX_NODES} узлов не поддерживается"
            ));
        }
        let idx = self.nodes.len();
        self.nodes.push(Node {
            id: id.to_string(),
            label: label.unwrap_or_else(|| id.to_string()),
            shape,
        });
        self.index.insert(id.to_string(), idx);
        Ok(idx)
    }

    fn add_edge(&mut self, from: usize, to: usize, label: Option<String>) -> Result<(), String> {
        if self.edges.len() >= MAX_EDGES {
            return Err(format!(
                "слишком большая диаграмма: больше {MAX_EDGES} рёбер не поддерживается"
            ));
        }
        self.edges.push(Edge { from, to, label });
        Ok(())
    }
}

/// Разбить исходник на операторы: переводы строк и `;` — разделители,
/// `%%` открывает комментарий до конца строки.
fn split_statements(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in source.lines() {
        // Комментарий mermaid: всё после `%%` игнорируем.
        let line = match line.find("%%") {
            Some(pos) => &line[..pos],
            None => line,
        };
        for stmt in line.split(';') {
            let stmt = stmt.trim();
            if !stmt.is_empty() {
                out.push(stmt.to_string());
            }
        }
    }
    out
}

/// Разобрать заголовок: `graph TD` / `graph LR` (синонимы `flowchart`, `TB`).
fn parse_header(stmt: &str) -> Result<Direction, String> {
    let mut parts = stmt.split_whitespace();
    let keyword = parts.next().unwrap_or("");
    if keyword != "graph" && keyword != "flowchart" {
        return Err(format!(
            "строка «{stmt}»: ожидался заголовок `graph TD` или `graph LR`"
        ));
    }
    match parts.next() {
        Some("TD") | Some("TB") => Ok(Direction::TopDown),
        Some("LR") => Ok(Direction::LeftRight),
        Some(other) => Err(format!(
            "строка «{stmt}»: направление «{other}» не поддерживается (только TD/LR)"
        )),
        None => Err(format!("строка «{stmt}»: не указано направление (TD/LR)")),
    }
}

/// Разобрать спецификацию узла в начале `s`: идентификатор и необязательную
/// метку в скобках. Возвращает (id, метка, форма, остаток строки).
fn parse_node_spec(s: &str) -> Result<(String, Option<String>, Shape, &str), String> {
    let id_len = s
        .char_indices()
        .take_while(|(_, c)| c.is_alphanumeric() || *c == '_' || *c == '-')
        .map(|(i, c)| i + c.len_utf8())
        .last()
        .unwrap_or(0);
    if id_len == 0 {
        return Err(format!("фрагмент «{s}»: ожидался идентификатор узла"));
    }
    let id = &s[..id_len];
    let rest = &s[id_len..];
    let (shape, open, close) = match rest.chars().next() {
        Some('[') => (Shape::Rect, '[', ']'),
        Some('(') => (Shape::Round, '(', ')'),
        Some('{') => (Shape::Diamond, '{', '}'),
        _ => return Ok((id.to_string(), None, Shape::Rect, rest)),
    };
    let body = &rest[open.len_utf8()..];
    // Метка в кавычках может содержать закрывающую скобку: A["a ] b"].
    let (label, after) = if let Some(stripped) = body.strip_prefix('"') {
        match stripped.find('"') {
            Some(end) => {
                let tail = &stripped[end + 1..];
                match tail.strip_prefix(close) {
                    Some(tail) => (&stripped[..end], tail),
                    None => {
                        return Err(format!("фрагмент «{s}»: после кавычек ожидалась «{close}»"))
                    }
                }
            }
            None => return Err(format!("фрагмент «{s}»: незакрытая кавычка в метке")),
        }
    } else {
        match body.find(close) {
            Some(end) => (&body[..end], &body[end + close.len_utf8()..]),
            None => return Err(format!("фрагмент «{s}»: незакрытая «{open}»")),
        }
    };
    let label = label.trim();
    if label.is_empty() {
        return Err(format!("фрагмент «{s}»: пустая метка узла"));
    }
    Ok((id.to_string(), Some(label.to_string()), shape, after))
}

/// Разобрать необязательную метку ребра после `-->`: `|label|`.
fn parse_edge_label(s: &str) -> (Option<String>, &str) {
    match s.strip_prefix('|') {
        Some(rest) => match rest.find('|') {
            Some(end) => {
                let label = rest[..end].trim();
                let label = if label.is_empty() {
                    None
                } else {
                    Some(label.to_string())
                };
                (label, &rest[end + 1..])
            }
            // Незакрытая `|` — считаем остаток меткой без неё (мягкий разбор
            // недопустим: вернём как есть, пусть разбор узла честно упадёт).
            None => (None, s),
        },
        None => (None, s),
    }
}

/// Разобрать один оператор: объявление узла или цепочка рёбер.
fn parse_statement(stmt: &str, graph: &mut Graph) -> Result<(), String> {
    let (id, label, shape, mut rest) =
        parse_node_spec(stmt).map_err(|e| format!("строка «{stmt}»: {e}"))?;
    let mut prev = graph.add_node(&id, label, shape)?;
    rest = rest.trim_start();
    while let Some(after_arrow) = rest.strip_prefix("-->") {
        let (edge_label, after_label) = parse_edge_label(after_arrow);
        let (id, label, shape, tail) = parse_node_spec(after_label.trim_start())
            .map_err(|e| format!("строка «{stmt}»: {e}"))?;
        let to = graph.add_node(&id, label, shape)?;
        graph.add_edge(prev, to, edge_label)?;
        prev = to;
        rest = tail.trim_start();
    }
    if !rest.is_empty() {
        return Err(format!("строка «{stmt}»: неожиданный хвост «{rest}»"));
    }
    Ok(())
}

/// Разобрать исходник mermaid-диаграммы. Ошибка всегда содержит строку,
/// на которой разбор споткнулся.
pub fn parse(source: &str) -> Result<Graph, String> {
    let statements = split_statements(source);
    let Some(first) = statements.first() else {
        return Err("пустой блок: ожидался заголовок `graph TD` или `graph LR`".to_string());
    };
    let direction = parse_header(first)?;
    let mut graph = Graph {
        direction,
        nodes: Vec::new(),
        edges: Vec::new(),
        index: HashMap::new(),
    };
    for stmt in &statements[1..] {
        parse_statement(stmt, &mut graph)?;
    }
    Ok(graph)
}

/// Раскладка по слоям: BFS-глубина от корней (узлы без входящих рёбер;
/// если таких нет — граф в цикле, корнем становится первый узел).
/// Множество посещённых гарантирует завершение на циклах; недостижимые
/// узлы попадают в финальный слой в порядке объявления.
fn layer_nodes(graph: &Graph) -> Vec<Vec<usize>> {
    let n = graph.nodes.len();
    let mut indeg = vec![0usize; n];
    for e in &graph.edges {
        indeg[e.to] += 1;
    }
    let mut depth = vec![usize::MAX; n];
    let mut queue: VecDeque<usize> = VecDeque::new();
    let mut has_root = false;
    for (i, d) in indeg.iter().enumerate() {
        if *d == 0 {
            depth[i] = 0;
            queue.push_back(i);
            has_root = true;
        }
    }
    if !has_root && n > 0 {
        depth[0] = 0;
        queue.push_back(0);
    }
    while let Some(u) = queue.pop_front() {
        for e in graph.edges.iter().filter(|e| e.from == u) {
            if depth[e.to] == usize::MAX {
                depth[e.to] = depth[u] + 1;
                queue.push_back(e.to);
            }
        }
    }
    let max_reachable = depth
        .iter()
        .copied()
        .filter(|d| *d != usize::MAX)
        .max()
        .unwrap_or(0);
    let mut layers: Vec<Vec<usize>> = vec![Vec::new(); max_reachable + 1];
    let mut lost: Vec<usize> = Vec::new();
    for (i, d) in depth.iter().enumerate() {
        if *d == usize::MAX {
            lost.push(i);
        } else {
            layers[*d].push(i);
        }
    }
    if !lost.is_empty() {
        layers.push(lost);
    }
    layers
}

/// Ширина бокса узла в символах: метка + рамка + внутренние пробелы.
fn node_width(graph: &Graph, idx: usize) -> usize {
    graph.nodes[idx].label.chars().count() + 4
}

/// Простой символьный холст: кладём глифы, в конце собираем строки.
struct Canvas {
    w: usize,
    h: usize,
    cells: Vec<Vec<char>>,
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Canvas {
            w,
            h,
            cells: vec![vec![' '; w]; h],
        }
    }

    fn put(&mut self, x: usize, y: usize, ch: char) {
        if x < self.w && y < self.h {
            self.cells[y][x] = ch;
        }
    }

    fn text(&mut self, x: usize, y: usize, s: &str) {
        for (i, ch) in s.chars().enumerate() {
            self.put(x + i, y, ch);
        }
    }

    fn lines(self) -> Vec<String> {
        self.cells
            .iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect()
    }
}

/// Отрисовать бокс узла левым верхним углом в (x, y). Возвращает ширину.
fn draw_box(canvas: &mut Canvas, graph: &Graph, idx: usize, x: usize, y: usize) -> usize {
    let node = &graph.nodes[idx];
    let w = node_width(graph, idx);
    let (tl, tr, bl, br, side_l, side_r) = match node.shape {
        Shape::Rect => ('┌', '┐', '└', '┘', '│', '│'),
        Shape::Round => ('╭', '╮', '╰', '╯', '│', '│'),
        Shape::Diamond => ('╱', '╲', '╲', '╱', '<', '>'),
    };
    canvas.put(x, y, tl);
    canvas.text(x + 1, y, &"─".repeat(w - 2));
    canvas.put(x + w - 1, y, tr);
    canvas.put(x, y + 1, side_l);
    canvas.text(x + 2, y + 1, &node.label);
    canvas.put(x + w - 1, y + 1, side_r);
    canvas.put(x, y + 2, bl);
    canvas.text(x + 1, y + 2, &"─".repeat(w - 2));
    canvas.put(x + w - 1, y + 2, br);
    w
}

/// TD-раскладка: слои друг под другом, каждый слой центрирован,
/// стрелки — вертикальные, опускаются в центр верхней кромки узла-цели
/// (так несколько детей одного родителя получают разные стрелки).
fn render_td(graph: &Graph, layers: &[Vec<usize>]) -> Vec<String> {
    // Ширина слоя: сумма ширин боксов + промежутки в 4 символа.
    let layer_widths: Vec<usize> = layers
        .iter()
        .map(|layer| {
            layer.iter().map(|&i| node_width(graph, i)).sum::<usize>()
                + 4 * layer.len().saturating_sub(1)
        })
        .collect();
    let mut canvas_w = layer_widths.iter().copied().max().unwrap_or(1).max(1);
    // Центры узлов нужны заранее, чтобы учесть вылезающие вправо метки рёбер.
    let centers = td_centers(graph, layers, &layer_widths, canvas_w);
    for e in &graph.edges {
        if let Some(label) = &e.label {
            let need = centers[e.to] + 2 + label.chars().count();
            canvas_w = canvas_w.max(need);
        }
    }
    let centers = td_centers(graph, layers, &layer_widths, canvas_w);
    let canvas_h = layers.len() * LAYER_STEP - 2;
    let mut canvas = Canvas::new(canvas_w, canvas_h);
    for (k, layer) in layers.iter().enumerate() {
        let mut x = (canvas_w - layer_widths[k]) / 2;
        let y = k * LAYER_STEP;
        for &i in layer {
            let w = draw_box(&mut canvas, graph, i, x, y);
            x += w + 4;
        }
    }
    // Стрелки только между соседними слоями; обратные/поперечные рёбра
    // в v1 не рисуем (ограничение задокументировано). Ось стрелки — центр
    // узла-цели, поэтому строка стрелки лежит в зазоре над её слоем.
    let depths = node_depths(layers, graph.nodes.len());
    for e in &graph.edges {
        if depths[e.to] != depths[e.from] + 1 {
            continue;
        }
        let x = centers[e.to];
        let y = depths[e.from] * LAYER_STEP + BOX_H;
        canvas.put(x, y, '│');
        if let Some(label) = &e.label {
            canvas.text(x + 2, y, label);
        }
        canvas.put(x, y + 1, '▼');
    }
    canvas.lines()
}

/// X-координаты центров узлов при заданной ширине холста.
fn td_centers(
    graph: &Graph,
    layers: &[Vec<usize>],
    layer_widths: &[usize],
    canvas_w: usize,
) -> Vec<usize> {
    let mut centers = vec![0usize; graph.nodes.len()];
    for (k, layer) in layers.iter().enumerate() {
        let mut x = (canvas_w - layer_widths[k]) / 2;
        for &i in layer {
            centers[i] = x + node_width(graph, i) / 2;
            x += node_width(graph, i) + 4;
        }
    }
    centers
}

/// Глубина каждого узла по слоям (для фильтра «соседних» рёбер).
fn node_depths(layers: &[Vec<usize>], n: usize) -> Vec<usize> {
    let mut depths = vec![0usize; n];
    for (k, layer) in layers.iter().enumerate() {
        for &i in layer {
            depths[i] = k;
        }
    }
    depths
}

/// LR-раскладка: слои — колонки слева направо, узлы в колонке стопкой,
/// стрелки — горизонтальные от правой кромки источника.
fn render_lr(graph: &Graph, layers: &[Vec<usize>]) -> Vec<String> {
    let col_widths: Vec<usize> = layers
        .iter()
        .map(|layer| {
            layer
                .iter()
                .map(|&i| node_width(graph, i))
                .max()
                .unwrap_or(1)
        })
        .collect();
    let depths = node_depths(layers, graph.nodes.len());
    // Ширина зазора между колонками: под самую длинную метку ребра в этом
    // зазоре, минимум 6 (четыре линии + стрелка + воздух).
    let mut gaps = vec![0usize; layers.len().saturating_sub(1)];
    for e in &graph.edges {
        if depths[e.to] == depths[e.from] + 1 && depths[e.from] < gaps.len() {
            let need = e.label.as_ref().map_or(0, |l| l.chars().count()) + 4;
            gaps[depths[e.from]] = gaps[depths[e.from]].max(need.max(6));
        }
    }
    let mut col_x = vec![0usize; layers.len()];
    for k in 1..layers.len() {
        col_x[k] = col_x[k - 1] + col_widths[k - 1] + gaps[k - 1];
    }
    let canvas_w = col_x.last().copied().unwrap_or(0) + col_widths.last().copied().unwrap_or(1);
    // Высота колонки: боксы стопкой с пустой строкой между ними.
    let col_heights: Vec<usize> = layers
        .iter()
        .map(|layer| layer.len() * BOX_H + layer.len().saturating_sub(1))
        .collect();
    let canvas_h = col_heights.iter().copied().max().unwrap_or(BOX_H);
    let mut canvas = Canvas::new(canvas_w, canvas_h);
    let mut mid_y = vec![0usize; graph.nodes.len()];
    for (k, layer) in layers.iter().enumerate() {
        let mut y = (canvas_h - col_heights[k]) / 2;
        for &i in layer {
            draw_box(&mut canvas, graph, i, col_x[k], y);
            mid_y[i] = y + 1;
            y += BOX_H + 1;
        }
    }
    for e in &graph.edges {
        if depths[e.to] != depths[e.from] + 1 {
            continue;
        }
        let x_from = col_x[depths[e.from]] + col_widths[depths[e.from]];
        let x_to = col_x[depths[e.to]];
        let row1 = mid_y[e.from];
        let row2 = mid_y[e.to];
        if row1 == row2 {
            // Цель на той же горизонтали — прямая стрелка.
            for x in x_from..x_to.saturating_sub(1) {
                canvas.put(x, row1, '─');
            }
            canvas.put(x_to - 1, row1, '▶');
        } else {
            // Цель выше/ниже: Г-образная трасса — вправо до середины
            // зазора, по вертикали, снова вправо в бокс цели.
            let xm = (x_from + x_to) / 2;
            for x in x_from..=xm {
                canvas.put(x, row1, '─');
            }
            let (top, bottom) = (row1.min(row2), row1.max(row2));
            for y in (top + 1)..bottom {
                canvas.put(xm, y, '│');
            }
            // Углы: слева-вниз/вверх и дальше направо.
            canvas.put(xm, row1, if row2 > row1 { '┐' } else { '┘' });
            canvas.put(xm, row2, if row2 > row1 { '└' } else { '┌' });
            for x in (xm + 1)..x_to.saturating_sub(1) {
                canvas.put(x, row2, '─');
            }
            canvas.put(x_to - 1, row2, '▶');
        }
    }
    // Сначала все трассы, потом все метки: иначе линия соседнего ребра
    // из того же узла затирает уже напечатанный текст.
    for e in &graph.edges {
        if depths[e.to] != depths[e.from] + 1 {
            continue;
        }
        let x_from = col_x[depths[e.from]] + col_widths[depths[e.from]];
        let row1 = mid_y[e.from];
        if let Some(label) = &e.label {
            canvas.text(x_from + 1, row1, label);
        }
    }
    canvas.lines()
}

/// Отрисовать разобранный граф в текстовые строки.
pub fn render(graph: &Graph) -> Vec<String> {
    if graph.nodes.is_empty() {
        return vec!["(пустая диаграмма)".to_string()];
    }
    let layers = layer_nodes(graph);
    match graph.direction {
        Direction::TopDown => render_td(graph, &layers),
        Direction::LeftRight => render_lr(graph, &layers),
    }
}

/// Разобрать и отрисовать исходник одним вызовом — точка интеграции
/// для markdown-пути TUI.
pub fn render_source(source: &str) -> Result<Vec<String>, String> {
    let graph = parse(source)?;
    Ok(render(&graph))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn render_ok(source: &str) -> Vec<String> {
        render_source(source).expect("диаграмма должна разбираться")
    }

    #[test]
    fn td_graph_renders_boxes_labels_and_edge_label() {
        let lines = render_ok("graph TD\nA[Начало] -->|да| B{Решение}\nB --> C(Конец)\nA --> C");
        let text = lines.join("\n");
        assert!(text.contains('┌'), "рамки: {text}");
        assert!(text.contains('▼'), "стрелки: {text}");
        assert!(text.contains("Начало"), "метка узла: {text}");
        assert!(text.contains("Решение"), "метка ромба: {text}");
        assert!(text.contains("Конец"), "метка скругления: {text}");
        assert!(text.contains("да"), "метка ребра: {text}");
        // Узел «Решение» объявлен ромбом — ждём характерные углы.
        assert!(text.contains('╱'), "ромб: {text}");
    }

    #[test]
    fn lr_graph_renders_horizontally() {
        let lines = render_ok("graph LR\nA[Старт] --> B[Финиш]");
        let text = lines.join("\n");
        assert!(text.contains('▶'), "горизонтальная стрелка: {text}");
        // Оба узла на одной горизонтали: «Старт» левее «Финиш» в одной строке.
        let mid = lines
            .iter()
            .find(|l| l.contains("Старт"))
            .expect("строка с узлом");
        assert!(mid.contains("Финиш"), "узлы в одной строке: {mid}");
        assert!(mid.find("Старт").unwrap() < mid.find("Финиш").unwrap());
    }

    #[test]
    fn chained_edges_and_implicit_nodes() {
        let graph = parse("graph TD\nA --> B --> C").expect("цепочка");
        assert_eq!(graph.nodes.len(), 3, "неявные узлы созданы");
        assert_eq!(graph.edges.len(), 2, "два ребра из цепочки");
        let lines = render(&graph);
        let text = lines.join("\n");
        // Метки по умолчанию — идентификаторы; вертикальная цепочка.
        for id in ["A", "B", "C"] {
            assert!(text.contains(id), "узел {id}: {text}");
        }
        assert_eq!(text.matches('▼').count(), 2, "две стрелки: {text}");
    }

    #[test]
    fn cycle_does_not_hang() {
        // Цикл A → B → A: раскладка обязана завершиться и что-то отрисовать.
        let lines = render_ok("graph TD\nA[А] --> B[Б]\nB --> A");
        let text = lines.join("\n");
        assert!(text.contains('А') && text.contains('Б'), "оба узла: {text}");
    }

    #[test]
    fn semicolons_and_comments_work() {
        let graph = parse("graph TD; A --> B %% комментарий\nB --> C;").expect("разбор");
        assert_eq!(graph.nodes.len(), 3);
        assert_eq!(graph.edges.len(), 2);
    }

    #[test]
    fn quoted_label_allows_brackets() {
        let graph = parse("graph TD\nA[\"метка с ] скобкой\"] --> B").expect("разбор");
        assert_eq!(graph.nodes[0].label, "метка с ] скобкой");
    }

    #[test]
    fn garbage_returns_err_without_panic() {
        assert!(parse("это вообще не mermaid").is_err());
        assert!(parse("graph TD\nA ~~~ B").is_err());
        assert!(parse("graph TD\nA[ --> B").is_err());
        assert!(parse("graph XX\nA --> B").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn oversized_graph_returns_honest_err() {
        // 60 узлов при потолке 50 — честная ошибка, не молчаливая обрезка.
        let mut src = String::from("graph TD\n");
        for i in 0..60 {
            src.push_str(&format!("N{i} --> N{}\n", i + 1));
        }
        let err = parse(&src).expect_err("должна быть ошибка размера");
        assert!(err.contains("50"), "говорящая ошибка: {err}");
    }

    #[test]
    fn render_source_integration() {
        // Точка интеграции: исходник fenced-блока → строки диаграммы.
        let lines = render_source("graph LR\nX --> Y").expect("рендер");
        assert!(lines.iter().any(|l| l.contains('▶')));
    }
}
