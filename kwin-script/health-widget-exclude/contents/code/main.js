// Помечает окно health-widget как исключённое из захвата экрана (KDE Plasma 6.6+)
// и печатает геометрию окон виджета в журнал: при появлении окна и по концу
// интерактивного перетаскивания. Виджет слушает journalctl -f и так узнаёт свою
// позицию без периодического опроса композитора.
//
// Зачем exclude: на Wayland нет клиентского флага «не захватывать это окно», но у KWin
// с 6.6 есть свойство окна excludeFromCapture (пункт меню «Hide from Screencast»).
// Оконных правил для него в 6.6 ещё нет (появятся в 6.7), поэтому выставляем свойство
// скриптом: виджет остаётся видимым локально, но не попадает в screencast / шаринг.
function announce(w) {
    var g = w.frameGeometry;
    if (String(w.caption) === "hw-clip") {
        print("HWC-GEOM " + Math.round(g.x) + " " + Math.round(g.y));
    } else if (String(w.caption) === "hw-chat") {
        print("HWCH-GEOM " + Math.round(g.x) + " " + Math.round(g.y));
    } else if (String(w.caption) === "hw-web") {
        print("HWWEB-GEOM " + Math.round(g.x) + " " + Math.round(g.y));
    } else {
        print("HW-GEOM x=" + Math.round(g.x) + " y=" + Math.round(g.y));
    }
}

function apply(w) {
    if (w && w.resourceClass && String(w.resourceClass) === "health-widget") {
        w.excludeFromCapture = true;
        announce(w);
        if (w.interactiveMoveResizeFinished) {
            w.interactiveMoveResizeFinished.connect(function () { announce(w); });
        }
    }
}

// Уже открытые окна (Plasma 6: windowList; старое API: clientList).
var list = workspace.windowList ? workspace.windowList()
         : (workspace.clientList ? workspace.clientList() : []);
for (var i = 0; i < list.length; i++) {
    apply(list[i]);
}

// Будущие окна — перезапуск виджета и т.п.
if (workspace.windowAdded) {
    workspace.windowAdded.connect(apply);
} else if (workspace.clientAdded) {
    workspace.clientAdded.connect(apply);
}
