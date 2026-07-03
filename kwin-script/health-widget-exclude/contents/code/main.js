// Помечает окно health-widget как исключённое из захвата экрана (KDE Plasma 6.6+).
//
// Зачем: на Wayland нет клиентского флага «не захватывать это окно», но у KWin с 6.6
// есть свойство окна excludeFromCapture (пункт меню «Hide from Screencast»). Оконных
// правил для него в 6.6 ещё нет (появятся в 6.7), поэтому выставляем свойство скриптом:
// виджет остаётся видимым локально, но не попадает в screencast / запись / шаринг.
function apply(w) {
    if (w && w.resourceClass && String(w.resourceClass) === "health-widget") {
        w.excludeFromCapture = true;
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
