#include "mainwindow.h"

#include <QAbstractItemModel>
#include <QComboBox>
#include <QCoreApplication>
#include <QDir>
#include <QDragEnterEvent>
#include <QDropEvent>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
#include <QHBoxLayout>
#include <QJsonArray>
#include <QJsonObject>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QListView>
#include <QMimeData>
#include <QPushButton>
#include <QScrollBar>
#include <QStackedWidget>
#include <QTimer>
#include <QUrl>
#include <QVBoxLayout>

#include "drivelist.h"
#include "iconcache.h"
#include "rowdelegate.h"
#include "tagdialog.h"
#include "theme.h"

namespace {

/* The file the headless transfer steps move and copy. Named for what it is so a person who finds it
 * left behind can tell it from their own, and so the cleanup step's log line reads as plainly as the
 * creation one. */
const char *const kDragProbe = "fam-self-check-drag.txt";

} /* namespace */

QString MainWindow::folderKey(const QString &path) {
    return QDir::cleanPath(QDir::fromNativeSeparators(path)).toLower();
}

MainWindow::MainWindow(const QString &configPath, const QString &initialQuery, QWidget *parent)
    : QWidget(parent) {
    m_clock.start();
    QString error;
    m_bridge = std::make_unique<FamBridge>(configPath, &error);
    qInfo("core handle ready %lldms after the window constructor started", m_clock.elapsed());
    if (!m_bridge->healthy()) {
        m_status = new QLabel(QStringLiteral("core unavailable: %1").arg(error), this);
        auto *layout = new QVBoxLayout(this);
        layout->addWidget(m_status);
        return;
    }

    /* The kind names come from the core, in the order its records' `kind` byte means them, and with
     * them the backends it has and which can answer now. The last two are logged rather than kept:
     * availability is probed in the background and changes while the window is open, so the only
     * honest reading of it is a fresh one - which is what `searchBackendNote` does per search. */
    QStringList backends, available;
    m_bridge->coreBackends(&m_kinds, &backends, &available, &error);
    qInfo("search backends: %s; answering now: %s", qUtf8Printable(backends.join(QStringLiteral(", "))),
          available.isEmpty() ? qUtf8Printable(QStringLiteral("none"))
                              : qUtf8Printable(available.join(QStringLiteral(", "))));

    /* The icon layer outlives every column and is shared by all of them: one worker thread, one
     * extension table, one cache. */
    m_icons = new IconCache(this);

    /* The page surface. `WA_StyledBackground` is deliberately not set: measured on the design
     * sample's own screenshot, `QWidget#page` paints (245,245,245) at a point outside every child
     * widget without it, because neither window overrides `paintEvent` and the style sheet style
     * therefore owns the background. */
    setObjectName(QString::fromLatin1(Theme::pageName()));

    m_search = new QLineEdit(this);
    m_search->setObjectName(QString::fromLatin1(Theme::searchName()));
    m_search->setPlaceholderText(QStringLiteral("name terms — every word must appear"));
    m_search->setClearButtonEnabled(true);

    m_kind = new QComboBox(this);
    m_kind->addItem(QStringLiteral("any kind"), QString());
    for (const QString &kind : m_kinds) {
        m_kind->addItem(kind, kind);
    }

    m_scope = new QComboBox(this);
    m_scope->addItem(QStringLiteral("this folder"), true);
    m_scope->addItem(QStringLiteral("every root"), false);
    /* Meaningless until a column is on screen, so it cannot disagree with what is showing. */
    m_scope->setEnabled(false);

    auto *searchButton = new QPushButton(QStringLiteral("Search"), this);
    searchButton->setObjectName(QString::fromLatin1(Theme::primaryButtonName()));
    auto *openButton = new QPushButton(QStringLiteral("Open folder…"), this);
    auto *tagButton = new QPushButton(QStringLiteral("Tags…"), this);
    tagButton->setToolTip(
        QStringLiteral("Show, add and remove the tags on the selected row or folder"));
    /* The split is a display choice, so it is a button rather than a mode the window can be stuck in:
     * collapsed, this is one browsing surface - what a person who never wanted a second column has
     * always been looking at. */
    m_split = new QPushButton(QStringLiteral("Split"), this);
    m_split->setObjectName(QString::fromLatin1(Theme::toggleButtonName()));
    m_split->setCheckable(true);
    m_split->setToolTip(QStringLiteral("Show a second, independent column pane on the left"));
    connect(m_split, &QPushButton::toggled, this, [this](bool on) {
        m_left->setVisible(on);
        /* Collapsing onto the pane that is about to disappear would leave the toolbar pointed at
         * nothing, so the focus moves to the one that stays. */
        if (!on && m_active == m_left) {
            setActivePane(m_right);
        } else if (on && m_active == nullptr) {
            setActivePane(m_right);
        }
    });

    /* A container rather than a bare layout, because `QWidget#toolbar` needs something to match: the
     * strip's own background and its bottom split are what separates the controls from the columns. */
    auto *toolbar = new QWidget(this);
    toolbar->setObjectName(QString::fromLatin1(Theme::toolbarName()));
    auto *controls = new QHBoxLayout(toolbar);
    const int gutter = Theme::instance().tokens().md;
    controls->setContentsMargins(gutter, gutter, gutter, gutter);
    controls->addWidget(m_search, 1);
    controls->addWidget(m_kind);
    controls->addWidget(m_scope);
    controls->addWidget(searchButton);
    controls->addWidget(openButton);
    controls->addWidget(tagButton);
    controls->addWidget(m_split);

    /* Two panes, built the same way, and only one of them on screen until the split button is
     * pressed. The right one is the pane that has always been there; the left one is the new
     * optional neighbour, which is why collapsing shows exactly the old window rather than a
     * shrunken version of a new one. */
    m_left = new Pane(m_bridge.get(), m_icons, m_kinds, this);
    m_right = new Pane(m_bridge.get(), m_icons, m_kinds, this);
    m_left->hide();
    m_active = m_right;
    for (Pane *pane : {m_left, m_right}) {
        QObject::connect(pane, &Pane::activated, pane, [this, pane] { setActivePane(pane); });
        /* Both panes' columns hand their own folder to the same handler, which is the whole point of
         * building them the same way: a drop from the left onto the right is one call to `pathOp` with
         * the two paths the panes supplied, and nothing in it knows which side of the window is which. */
        QObject::connect(pane, &Pane::dropped, this, &MainWindow::handleDrop);
    }

    m_status = new QLabel(this);
    m_status->setObjectName(QString::fromLatin1(Theme::statusName()));
    m_status->setWordWrap(true);

    m_drives = new DriveList(this);
    connect(m_drives, &DriveList::drivePicked, this, [this](const QString &root) {
        QString error;
        if (!openFolder(root, &error)) {
            setStatus(error);
        }
    });

    auto *body = new QHBoxLayout;
    body->setContentsMargins(0, 0, 0, 0);
    body->setSpacing(0);
    body->addWidget(m_drives);
    body->addWidget(m_left, 1);
    body->addWidget(m_right, 1);

    auto *layout = new QVBoxLayout(this);
    layout->setContentsMargins(0, 0, 0, 0);
    layout->setSpacing(0);
    layout->addWidget(toolbar);
    layout->addLayout(body, 1);
    layout->addWidget(m_status);

    connect(searchButton, &QPushButton::clicked, this, &MainWindow::runSearch);
    connect(m_search, &QLineEdit::returnPressed, this, &MainWindow::runSearch);
    /* Emptying the box is "stop searching", not "search for nothing". */
    connect(m_search, &QLineEdit::textChanged, this, [this](const QString &text) {
        if (text.isEmpty()) {
            showFolder();
        }
    });
    connect(openButton, &QPushButton::clicked, this, &MainWindow::openFolderDialog);
    connect(tagButton, &QPushButton::clicked, this, &MainWindow::tagSelectedFile);

    QJsonValue stats;
    if (m_bridge->stats(&stats, &error)) {
        const QJsonObject object = stats.toObject();
        QStringList roots;
        for (const QJsonValue &root : object.value("roots").toArray()) {
            roots.append(root.toString());
        }
        setStatus(QStringLiteral("roots: %1 · files %2 · tags %3 · open a folder to browse")
                      .arg(roots.join(QStringLiteral(", ")))
                      .arg(object.value("files").toInt())
                      .arg(object.value("tags").toInt()));
    } else {
        setStatus(error);
    }

    qInfo("widgets built and shown %lldms after the window constructor started",
          m_clock.elapsed());
    m_timer = new QTimer(this);
    m_timer->setInterval(250);
    connect(m_timer, &QTimer::timeout, this, &MainWindow::pollChanges);
    m_timer->start();
    /* Everything the slots and the self-check steps touch exists from here on, and not before: the
     * early return above leaves the window with a status label and no chain. */
    m_healthy = true;

    if (!initialQuery.isEmpty()) {
        m_search->setText(initialQuery);
        runSearch();
    }
}

MainWindow::~MainWindow() {
    if (!m_healthy) {
        /* The panes were never built; there is nothing to close and their chains are null. */
        return;
    }
    /* The chains close the directories they hold, and that has to happen here rather than in their own
     * destructors: a QObject child is destroyed after this class's members, and `m_bridge` owns the
     * handle those calls need. Same reason for the search tokens. */
    for (Pane *pane : {m_left, m_right}) {
        pane->chain->clear();
        if (pane->searchToken != 0) {
            m_bridge->closeSearch(pane->searchToken);
            pane->searchToken = 0;
        }
    }
}

bool MainWindow::openFolder(const QString &dir, QString *error) {
    return openFolderIn(active(), dir, error);
}

bool MainWindow::openFolderIn(Pane *pane, const QString &dir, QString *error) {
    if (!m_healthy) {
        *error = QStringLiteral("core unavailable");
        return false;
    }
    if (!pane->chain->openRoot(dir)) {
        *error = QStringLiteral("cannot open %1").arg(QDir::toNativeSeparators(dir));
        return false;
    }
    if (pane->searchToken != 0) {
        /* A folder on screen replaces this pane's results, and the search that produced them is
         * cancelled rather than left running to fill a token nobody pages any more. The other pane's
         * search is none of this call's business. */
        m_bridge->cancelSearch(pane->searchToken);
        m_bridge->closeSearch(pane->searchToken);
        pane->searchToken = 0;
        pane->searchRunning = false;
    }
    pane->showFolder();
    if (pane == active()) {
        refreshScopeAvailability();
    }
    const FamWindow head = pane->chain->deepestHead();
    qInfo("folder on screen: %s at %lldms: state=%u total=%u generation=%llu",
          qUtf8Printable(QDir::toNativeSeparators(dir)), m_clock.elapsed(), head.state,
          head.totalRows, head.generation);
    /* Only the pane a person is looking at gets the status line. Writing it from the other one is how
     * the bottom of the window ends up describing a folder that is not what the toolbar will act on
     * next - which the headless run caught by itself, and then I had to read the log to believe. */
    if (pane == active()) {
        setStatus(folderStatus(dir, head, QStringLiteral("following changes")));
    }
    return true;
}

void MainWindow::openFolderDialog() {
    Pane *pane = active();
    const QString start =
        pane->chain->isEmpty() ? QDir::homePath() : pane->chain->deepestPath();
    const QString dir =
        QFileDialog::getExistingDirectory(this, QStringLiteral("Open folder"), start);
    if (dir.isEmpty()) {
        return;
    }
    QString error;
    if (!openFolder(dir, &error)) {
        setStatus(error);
    }
}

void MainWindow::showFolder() {
    Pane *pane = active();
    if (!m_healthy || pane->searchToken == 0) {
        return;
    }
    m_bridge->cancelSearch(pane->searchToken);
    m_bridge->closeSearch(pane->searchToken);
    pane->searchToken = 0;
    pane->searchRunning = false;
    if (pane->chain->isEmpty()) {
        /* Nothing to go back to: the results were a search over every root, and clearing the box
         * leaves an empty view rather than a result set nobody is paging. */
        pane->model->unbind();
        setStatus(QStringLiteral("type a name to search"));
        return;
    }
    pane->showFolder();
    refreshScopeAvailability();
    setStatus(folderStatus(pane->chain->deepestPath(), pane->chain->deepestHead(),
                           QStringLiteral("following changes")));
}

void MainWindow::refreshScopeAvailability() {
    m_scope->setEnabled(!active()->chain->isEmpty());
}

void MainWindow::setActivePane(Pane *pane) {
    if (pane == nullptr || pane == m_active) {
        return;
    }
    m_active = pane;
    /* The toolbar reads the pane that now owns it: the scope selector is enabled by *that* pane's
     * chain, and the status line is rewritten so it cannot keep describing the other one. */
    refreshScopeAvailability();
    if (pane->chain->isEmpty() && !pane->showingResults()) {
        setStatus(QStringLiteral("no folder open in this pane · type a name to search"));
    } else if (pane->showingResults()) {
        setStatus(QStringLiteral("%1 row(s) in the results of this pane")
                      .arg(pane->model->totalRows()));
    } else {
        setStatus(folderStatus(pane->chain->deepestPath(), pane->chain->deepestHead(),
                              QStringLiteral("following changes")));
    }
}

void MainWindow::toggleSplit() { m_split->click(); }

void MainWindow::runSearch() {
    if (!m_healthy) {
        return;
    }
    Pane *pane = active();
    QString error;
    const QString kind = m_kind->currentData().toString();
    /* The scope is the operator's, not a guess: "here" is what a person means after opening a folder,
     * and "everywhere" is what this box used to always mean. Either one alone is a regression. */
    const bool here = !pane->chain->isEmpty() && m_scope->currentData().toBool();
    const QString within = here ? pane->chain->deepestPath() : QString();
    quint64 token = 0;
    if (!m_bridge->beginSearch(m_search->text(), within, kind, 0, &token, &error)) {
        setStatus(error);
        return;
    }
    if (pane->searchToken != 0) {
        m_bridge->closeSearch(pane->searchToken);
    }
    pane->searchToken = token;
    pane->searchRunning = true;

    const quint64 held = token;
    FamBridge *bridge = m_bridge.get();
    /* The same windowed fetch a column gets, against a token whose rows are still arriving: the model
     * asks for a viewport, and the core answers with the prefix it has. */
    pane->model->bind(token, QString(), m_kinds,
                      [bridge, held](quint32 first, quint32 count, FamWindow *window) {
                          return bridge->searchPage(held, first, count, window, nullptr);
                      });
    FamWindow head;
    if (!m_bridge->searchStatus(token, &head, &error)) {
        setStatus(error);
        return;
    }
    pane->model->readHead(head);
    pane->showResults();
    refreshScopeAvailability();
    /* The line says which scope was actually asked for, because a search that quietly narrowed itself
     * is indistinguishable from one that found nothing elsewhere. */
    qInfo("search for %s in %s queued at %lldms: state=%u total=%u",
          qUtf8Printable(m_search->text()),
          here ? qUtf8Printable(QDir::toNativeSeparators(within))
               : qUtf8Printable(QStringLiteral("every configured root")),
          m_clock.elapsed(), head.state, head.totalRows);
    setStatus(QStringLiteral("searching for %1").arg(m_search->text()));
}

void MainWindow::pollChanges() {
    if (!m_healthy) {
        return;
    }
    /* Both panes are served on every beat, and both halves of each pane: the columns are polled even
     * while results are on screen, because a view nobody drains queues its cut positions for as long
     * as it lives. A hidden pane still gets polled too - its watch is live, and stopping here is how
     * a collapsed split comes back showing a stale folder. */
    for (Pane *pane : {m_left, m_right}) {
        const QString changed = pane->chain->poll();
        if (!changed.isEmpty() && pane == active() && !pane->showingResults()) {
            setStatus(folderStatus(pane->chain->deepestPath(), pane->chain->deepestHead(), changed));
        }
        if (pane->searchToken != 0 && pane->searchRunning) {
            pollSearch(pane);
        }
    }
}

void MainWindow::pollSearch(Pane *pane) {
    QString error;
    FamWindow polled;
    if (!m_bridge->searchStatus(pane->searchToken, &polled, &error)) {
        pane->searchRunning = false;
        setStatus(error);
        return;
    }
    pane->model->readHead(polled);
    /* A search the person is no longer looking at still has to be finished - its rows are being
     * fetched either way - but only the active pane gets the status line. */
    const bool shown = pane == active();
    if (polled.state == FAM_LOADING) {
        if (shown) {
            setStatus(QStringLiteral("%1 row(s) for %2 · still searching")
                          .arg(pane->model->totalRows())
                          .arg(m_search->text()));
        }
        return;
    }
    pane->searchRunning = false;
    const QString backend = searchBackendNote();
    const QString why = polled.state == FAM_FAILED
                            ? QStringLiteral(" · the search backend could not answer")
                            : (polled.partial() ? QStringLiteral(" · scope not fully covered")
                                                : QString());
    /* The headless gate reads this: rows arriving through the paged channel is the whole point of a v2
     * search, and a screenshot alone cannot tell a settled result set from an empty list. The backend
     * note rides along so the log says which engine produced the number. */
    qInfo("search settled at %lldms: %u row(s)%s%s", m_clock.elapsed(), polled.totalRows,
          qUtf8Printable(why), qUtf8Printable(backend));
    if (shown) {
        setStatus(QStringLiteral("%1 row(s) for %2%3%4")
                      .arg(pane->model->totalRows())
                      .arg(m_search->text())
                      .arg(why, backend));
    }
}

QString MainWindow::folderStatus(const QString &directory, const FamWindow &head,
                                 const QString &extra) const {
    QString text = QStringLiteral("%1 · %2 row(s)")
                       .arg(QDir::toNativeSeparators(directory))
                       .arg(head.totalRows);
    switch (head.state) {
    case FAM_READY:
        /* Ready with nothing in it is a claim about the folder. Ready with rows needs no word. */
        if (head.totalRows == 0) {
            text += QStringLiteral(" · empty");
        }
        break;
    case FAM_LOADING:
        text += QStringLiteral(" · reading");
        break;
    case FAM_STALE:
        text += QStringLiteral(" · may be out of date");
        break;
    case FAM_FAILED:
        text += QStringLiteral(" · cannot be read (permissions, deleted, or unreachable)");
        break;
    case FAM_EMPTY:
        /* The core's name for "nothing cached and nothing in flight", which is not the same claim as
         * "this folder is empty" - and the two used to look identical on screen. */
        text += QStringLiteral(" · not read yet");
        break;
    default:
        text += QStringLiteral(" · state %1").arg(head.state);
        break;
    }
    if (head.truncated()) {
        text += QStringLiteral(" · more pending");
    }
    if (head.lostEvents()) {
        text += QStringLiteral(" · events dropped");
    }
    if (!extra.isEmpty()) {
        text += QStringLiteral(" · %1").arg(extra);
    }
    return text;
}

QString MainWindow::searchBackendNote() {
    if (!m_healthy) {
        return QString();
    }
    QStringList kinds, backends, available;
    QString ignored;
    if (!m_bridge->coreBackends(&kinds, &backends, &available, &ignored)) {
        return QString();
    }
    if (available.contains(QStringLiteral("everything"))) {
        return QString();
    }
    /* The one sentence the interface could never say before: that the index is not answering and
     * every name search is walking the tree instead. It changes the expected latency by orders of
     * magnitude, so it belongs on screen rather than in a JSON payload nobody reads. */
    return QStringLiteral(" · enumerating for names (no index answering)");
}

void MainWindow::tagSelectedFile() {
    if (!m_healthy) {
        return;
    }
    Pane *pane = active();
    const bool showingResults = pane->showingResults();
    QString path;
    if (showingResults) {
        if (pane->results->selectionModel() == nullptr) {
            return;
        }
        path = pane->model->pathAt(pane->results->selectionModel()->currentIndex().row());
    } else {
        path = pane->chain->selectedPath();
    }
    if (path.isEmpty()) {
        setStatus(QStringLiteral("select a row first"));
        return;
    }

    /* The manager rather than a one-word prompt: the file's existing tags used to be invisible here,
     * and a tag could not be taken back off at all. */
    TagDialog dialog(m_bridge.get(), path, this);
    dialog.exec();
    /* A tag changes what the row displays, and a folder view learns that on its own; a search result
     * has to be re-run to keep its rows in step. */
    if (!dialog.changed()) {
        return;
    }
    if (showingResults) {
        runSearch();
    }
    setStatus(QStringLiteral("%1 tag(s) on %2")
                  .arg(dialog.tagNames().size())
                  .arg(QFileInfo(path).fileName()));
}

/* A drop is the one way a pointer can ask this window to change what is on disk, so this is where its
 * meaning is fixed: the destination is the folder the column shows, the item keeps its name, and the
 * transfer is the core's - which is the same `fam_svc_fs::transfer` the MCP tools go through, so the
 * rule that refuses to overwrite a file refuses a person's drag too, in the same words.
 *
 * `copy` came from the modifier the drop event carried: plain drag means move, Ctrl means copy. */
void MainWindow::handleDrop(const QString &folder, const QStringList &paths, bool copy) {
    if (!m_healthy) {
        return;
    }
    const QString verb = copy ? QStringLiteral("copied") : QStringLiteral("moved");
    int done = 0;
    int already = 0;
    QStringList refused;
    for (const QString &source : paths) {
        const QFileInfo info(source);
        const QString name = info.fileName();
        if (name.isEmpty()) {
            /* A volume root has no name of its own to put down inside another folder. That is a thing
             * the drag could not mean rather than a rule that said no, so it is worded as the former. */
            refused.append(QStringLiteral("%1 is not a file or folder with a name").arg(info.filePath()));
            continue;
        }
        if (folderKey(info.absolutePath()) == folderKey(folder)) {
            /* Dropped back where it came from. Skipping the call rather than making it is not only
             * cheaper: the core would answer SamePath, and "cannot move: the source and the
             * destination are the same path" is a bad thing to say about something a person did not ask. */
            ++already;
            continue;
        }
        QJsonValue result;
        QString error;
        if (m_bridge->pathOp(source, QDir(folder).filePath(name), copy, &result, &error)) {
            ++done;
        } else {
            refused.append(error);
        }
    }
    QStringList clauses;
    if (done > 0) {
        clauses.append(QStringLiteral("%1 %2 item(s) into %3")
                           .arg(verb)
                           .arg(done)
                           .arg(QDir::toNativeSeparators(folder)));
    }
    if (already > 0) {
        clauses.append(QStringLiteral("%1 item(s) were already in that folder").arg(already));
    }
    clauses.append(refused);
    /* The counts go to the log as well as the sentence to the status line, because the headless gate
     * reads words and a status bar is not something it can assert on. Both columns learn the new rows
     * from their own watches on the next beat - which is the whole reason a drop into a folder that is
     * *not* on screen is fine too: nothing has to be told. */
    qInfo("drop %s into %s: %d transferred, %d already there, %d refused: %s",
          qUtf8Printable(verb), qUtf8Printable(QDir::toNativeSeparators(folder)), done, already,
          refused.size(), qUtf8Printable(clauses.join(QStringLiteral("; "))));
    setStatus(clauses.isEmpty() ? QStringLiteral("the drop carried nothing to transfer")
                                : clauses.join(QStringLiteral("; ")));
}

void MainWindow::keyPressEvent(QKeyEvent *event) {
    const bool back = event->key() == Qt::Key_Backspace
                      || (event->key() == Qt::Key_Left
                          && event->modifiers().testFlag(Qt::AltModifier));
    Pane *pane = active();
    if (back && pane != nullptr && pane->chain != nullptr && !pane->chain->isEmpty()) {
        const QString path = pane->chain->goBack();
        refreshScopeAvailability();
        if (path.isEmpty()) {
            setStatus(QStringLiteral("no folder open"));
        } else {
            setStatus(
                folderStatus(path, pane->chain->deepestHead(), QStringLiteral("went up one")));
        }
        return;
    }
    QWidget::keyPressEvent(event);
}

void MainWindow::setStatus(const QString &text) {
    if (m_status != nullptr) {
        m_status->setText(text);
    }
}

void MainWindow::scrollForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    QListView *view = active()->chain == nullptr ? nullptr : active()->chain->deepestView();
    if (view == nullptr || view->verticalScrollBar() == nullptr) {
        return;
    }
    QScrollBar *bar = view->verticalScrollBar();
    const int max = bar->maximum();
    /* Nine stops then home: a viewport shows about seventeen rows, so fewer stops would leave bands
     * of the folder unpainted - and an extension nobody painted is an extension the icon worker was
     * never asked about, which is exactly what the HICON counters are meant to have exercised.
     * processEvents plus repaint is what makes each stop actually paint: a deferred paint would
     * collapse all nine into one paint of wherever the bar ended. The range is logged because a storm
     * over a zero range would prove nothing and look identical in the counters. */
    for (int step = 0; step <= 8; ++step) {
        bar->setValue(max * step / 8);
        QCoreApplication::processEvents();
        view->viewport()->repaint();
    }
    bar->setValue(0);
    QCoreApplication::processEvents();
    view->viewport()->repaint();
    qInfo("self-check scrolled a %d-step range, ended at %d", max, bar->value());
}

void MainWindow::createSelfCheckProbe() {
    if (!m_healthy) {
        qWarning("self-check mutation asked for without a working core");
        return;
    }
    const QString folder = active()->chain->deepestPath();
    if (folder.isEmpty()) {
        qWarning("self-check mutation asked for without a watched folder");
        return;
    }
    m_probe = QDir(folder).filePath(QStringLiteral("fam-self-check-probe.txt"));
    QFile file(m_probe);
    if (!file.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
        qWarning("self-check could not create %s: %s", qUtf8Printable(m_probe),
                 qUtf8Printable(file.errorString()));
        m_probe.clear();
        return;
    }
    file.write("probe");
    file.close();
    qInfo("self-check created %s", qUtf8Printable(QDir::toNativeSeparators(m_probe)));
}

void MainWindow::removeSelfCheckProbe() {
    /* Every file the run wrote into the watched tree, from whichever column it is in now: the transfer
     * steps move one and copy another, so a path remembered at creation time is not necessarily where
     * the file sits, and a copy leaves one at each end. */
    for (QString *path : {&m_probe, &m_draggedFrom, &m_draggedTo}) {
        if (path->isEmpty()) {
            continue;
        }
        /* "not there" rather than "yes": a move means the file stopped existing at the end it was
         * written at, and a cleanup that claimed to have removed it would be describing the far end
         * twice. */
        const QString verdict = !QFile::exists(*path)
                                    ? QStringLiteral("not there")
                                    : (QFile::remove(*path) ? QStringLiteral("yes")
                                                           : QStringLiteral("no"));
        qInfo("self-check removed %s: %s", qUtf8Printable(QDir::toNativeSeparators(*path)),
              qUtf8Printable(verdict));
        path->clear();
    }
}

/* The tag round trip, driven through the dialog's own operations because a modal dialog is the one
 * thing a run with no hands cannot drive. Every line is a claim about the wire, not about widgets:
 * the file's tags can be read, one can be added and read back, the store's own text rules reach the
 * interface, and a remove with nothing picked refuses *without* deleting what is already there. */
void MainWindow::tagsForSelfCheck() {
    if (!m_healthy || m_probe.isEmpty() || !QFile::exists(m_probe)) {
        qWarning("self-check tag step asked for without a file to tag");
        return;
    }
    TagDialog dialog(m_bridge.get(), m_probe, this);
    const QStringList started = dialog.tagNames();
    QString error;
    const bool added = dialog.addTag(QStringLiteral("self-check"), &error);
    dialog.reload();
    const QStringList afterAdd = dialog.tagNames();
    /* Over 64 characters: the store's rule, not the dialog's, and the sentence a person sees should
     * be the one the agent gets for the same attempt. */
    const bool blankRefused = !dialog.addTag(QString(65, u'x'), &error);
    const QString blankSentence = error;
    /* The footgun. An empty tag list on the wire deletes every tag the file carries, so this call
     * must refuse *and* leave `self-check` in place - the second half is the only proof it refused
     * before sending rather than after. */
    const bool emptied = dialog.removeTag(QString(), &error);
    const QStringList afterEmptyAttempt = dialog.tagNames();
    const bool removed = dialog.removeTag(QStringLiteral("self-check"), &error);
    dialog.reload();
    qInfo("self-check tags on %s: started=[%s] add=%d listed=[%s] remove-nothing=%d still=[%s]",
          qUtf8Printable(QFileInfo(m_probe).fileName()), qUtf8Printable(started.join(u',')),
          added ? 1 : 0, qUtf8Printable(afterAdd.join(u',')), emptied ? 1 : 0,
          qUtf8Printable(afterEmptyAttempt.join(u',')));
    qInfo("self-check tags: over-long tag refused=%d (\"%s\") remove=%d final=[%s]",
          blankRefused ? 1 : 0, qUtf8Printable(blankSentence), removed ? 1 : 0,
          qUtf8Printable(dialog.tagNames().join(u',')));
}

/* The dialog as a picture, because everything above asserts about the wire and nothing about whether
 * a person can read the result. Shown rather than constructed so the layout runs: a dialog that was
 * never laid out grabs as a stack of widgets at their preferred sizes, which would have proved the
 * opposite of what it looks like it proves. */
bool MainWindow::grabTagDialogForSelfCheck(const QString &imagePath) {
    if (!m_healthy || m_probe.isEmpty() || !QFile::exists(m_probe)) {
        qWarning("self-check dialog shot asked for without a file to tag");
        return false;
    }
    auto *dialog = new TagDialog(m_bridge.get(), m_probe, this);
    QString error;
    const bool seeded = dialog->addTag(QStringLiteral("self-check-shot"), &error);
    dialog->reload();
    dialog->show();
    QCoreApplication::processEvents();
    const bool saved = dialog->grab().save(imagePath);
    qInfo("self-check wrote the tag dialog to %s: seeded=%d listed=%d %s",
          qUtf8Printable(QDir::toNativeSeparators(imagePath)), seeded ? 1 : 0,
          dialog->tagNames().size(), saved ? "saved" : "FAILED to save");
    /* Back to however many tags the file started with, so the shot does not leave a mark. */
    dialog->removeTag(QStringLiteral("self-check-shot"), &error);
    delete dialog;
    return saved;
}

void MainWindow::runSelfCheckSearch(const QString &terms) {
    m_search->setText(terms);
    runSearch();
}

void MainWindow::clearSelfCheckSearch() {
    m_search->clear();
}

void MainWindow::descendForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    /* Depth before and after, rather than a yes/no: past the cap the chain stops growing on
     * purpose, and "did it grow" is the only question that distinguishes that from "the selection
     * handler stopped working". */
    const int before = active()->chain->depth();
    active()->chain->selectFirstFolder();
    qInfo("self-check descended: depth %d -> %d", before, active()->chain->depth());
}

void MainWindow::sortDeepestForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    const QString applied = active()->chain->sortDeepest();
    qInfo("self-check sorted the deepest column by %s", qUtf8Printable(applied));
}

void MainWindow::clickHeaderForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    const bool clicked = active()->chain->clickDeepestHeader();
    qInfo("self-check clicked the deepest column's header: %s, chain depth %d",
          clicked ? "yes" : "no", active()->chain->depth());
}

void MainWindow::pickFirstDriveForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    const QString root = m_drives->selectFirst();
    qInfo("self-check picked drive %s, chain depth %d", qUtf8Printable(root), active()->chain->depth());
}

void MainWindow::goUpForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    const QString remained = active()->chain->goBack();
    refreshScopeAvailability();
    qInfo("self-check went up to %s, chain depth %d",
          qUtf8Printable(QDir::toNativeSeparators(remained)), active()->chain->depth());
}

/* Opens the second pane and puts the watched folder in it. The point is the two numbers at the end:
 * with one chain the left pane could only ever mirror the right, and a gate that never opens the
 * second pane would ship it untested. */
void MainWindow::toggleSplitForSelfCheck(bool on) {
    if (!m_healthy) {
        return;
    }
    if (m_split->isChecked() != on) {
        m_split->click();
    }
    if (!on) {
        qInfo("self-check collapsed the split: left visible=%d active=%s", m_left->isVisible() ? 1 : 0,
              m_active == m_left ? "left" : "right");
        return;
    }
    const int rightBefore = m_right->chain->depth();
    QString error;
    if (!m_watch.isEmpty()) {
        openFolderIn(m_left, m_watch, &error);
    }
    /* The right pane keeps whatever the drive pick left it on: same depth, same folder. If opening
     * the left one moved the right, they are not two panes - they are one chain wearing two frames. */
    qInfo("self-check opened the split: left visible=%d left-depth=%d right-depth=%d (was %d) "
          "active=%s",
          m_left->isVisible() ? 1 : 0, m_left->chain->depth(), m_right->chain->depth(), rightBefore,
          m_active == m_left ? "left" : "right");
}

/* Puts a real drop on a real column view and asks the view what it did with it.
 *
 * It sends the pair a platform drag sends - an enter, then the drop - rather than the drop alone.
 * That is not ceremony: `QAbstractItemView` only routes a Drop to `dropEvent` once the view has been
 * put into drag state by an enter, so a drop posted on its own is answered by Qt's default and never
 * reaches this window's handler at all. The first version of this posted the drop alone, the handler
 * did not run, and the step still reported `taken=1` - a gate passing on an event nobody handled.
 *
 * The events go to the viewport, because that is where the platform delivers them: the scroll area's
 * own filter turns them into the view's `dragEnterEvent` and `dropEvent` virtuals, so the code this
 * reaches is the code a person's drag reaches. What it does not fake is the drag itself - a mouse
 * gesture no headless run can perform - and `outcome` carries back the action the view answered with,
 * which is the difference between "a handler ran somewhere" and "this view took the drop as a copy". */
bool MainWindow::postDrop(FileView *view, const QString &source, bool copy, QString *outcome) {
    if (view == nullptr || view->viewport() == nullptr) {
        *outcome = QStringLiteral("no view");
        return false;
    }
    const std::unique_ptr<QMimeData> data(FileView::mimeDataForPaths(QStringList{source}));
    /* Somewhere inside the viewport, and it does not matter where: the destination is the folder the
     * column owns rather than the row the point happens to be over. */
    const QPoint at(view->viewport()->width() / 2, view->viewport()->height() / 2);
    const Qt::DropActions wanted = copy ? Qt::CopyAction : Qt::MoveAction;
    const Qt::KeyboardModifiers keys = copy ? Qt::ControlModifier : Qt::NoModifier;
    QDragEnterEvent enter(at, wanted, data.get(), Qt::LeftButton, keys);
    QCoreApplication::sendEvent(view->viewport(), &enter);
    QDropEvent event(at, wanted, data.get(), Qt::LeftButton, keys);
    QCoreApplication::sendEvent(view->viewport(), &event);
    switch (event.dropAction()) {
    case Qt::CopyAction: *outcome = QStringLiteral("copy"); break;
    case Qt::MoveAction: *outcome = QStringLiteral("move"); break;
    default: *outcome = QStringLiteral("ignored"); break;
    }
    return event.dropAction() == wanted;
}

/* The border, from the two sides it has to be refused on. A payload of ours is answered; a standard
 * `text/uri-list` - which is what anything outside this window would carry - is not, so there is no
 * second, unaccounted-for way for a file to be moved by dropping it here. */
void MainWindow::checkBorderForSelfCheck(FileView *view, const QString &source) {
    if (view == nullptr || view->viewport() == nullptr) {
        qWarning("self-check had no view to check the border of");
        return;
    }
    /* An integer position here where `QDropEvent` takes a floating one, which is the sort of thing a
     * compile tells you before a note does. */
    const QPoint at(view->viewport()->width() / 2, view->viewport()->height() / 2);
    const std::unique_ptr<QMimeData> ours(FileView::mimeDataForPaths(QStringList{source}));
    QDragEnterEvent ownEvent(at, Qt::CopyAction | Qt::MoveAction, ours.get(), Qt::LeftButton,
                             Qt::NoModifier);
    QCoreApplication::sendEvent(view->viewport(), &ownEvent);

    auto foreign = std::make_unique<QMimeData>();
    foreign->setUrls({QUrl::fromLocalFile(source)});
    QDragEnterEvent foreignEvent(at, Qt::CopyAction | Qt::MoveAction, foreign.get(), Qt::LeftButton,
                                 Qt::NoModifier);
    QCoreApplication::sendEvent(view->viewport(), &foreignEvent);

    qInfo("self-check border of %s: ours %s, text/uri-list %s",
          qUtf8Printable(QDir::toNativeSeparators(view->dropFolder())),
          ownEvent.isAccepted() ? "accepted" : "refused",
          foreignEvent.isAccepted() ? "accepted" : "refused");
}

/* The payload a person's drag would carry, made the way a real drag makes it - out of a row that is
 * actually listed, through `FamListModel::mimeData` - and read back with the decoder the drop side
 * uses. A row that is already on screen rather than the file this step is about to write, because the
 * watch has not had a beat to learn about that one yet; the claim here is about the format, not about
 * which file came through it. */
void MainWindow::reportDragPayload(FileView *view) {
    const QAbstractItemModel *model = view == nullptr ? nullptr : view->model();
    const QModelIndex row = model == nullptr ? QModelIndex() : model->index(0, 0);
    if (!row.isValid()) {
        qWarning("self-check had no listed row to encode");
        return;
    }
    const QString path = row.data(FamListModel::PathRole).toString();
    const std::unique_ptr<QMimeData> data(model->mimeData(QModelIndexList{row}));
    const QStringList back = FileView::pathsOf(data.get());
    qInfo("self-check encoded a listed row as a drag: %s carried %d path(s), same=%d",
          qUtf8Printable(QFileInfo(path).fileName()), back.size(),
          back.size() == 1 && back.first() == path ? 1 : 0);
}

void MainWindow::prepareTransferForSelfCheck() {
    if (!m_healthy || m_watch.isEmpty()) {
        qWarning("self-check transfer asked for without a watched folder");
        return;
    }
    if (!m_split->isChecked()) {
        m_split->click();
    }
    /* Both ends inside the tree `--watch` named. The drive pick earlier in the run had left the right
     * pane on a volume root, and a test that wrote there would be writing somewhere nobody asked this
     * program to touch. The left goes one level deeper than the right so that the two ends are two
     * folders - and one chain's depth is what the other one's is not. */
    QString error;
    if (!openFolderIn(m_right, m_watch, &error)) {
        qWarning("self-check could not open %s in the right pane: %s",
                 qUtf8Printable(QDir::toNativeSeparators(m_watch)), qUtf8Printable(error));
        return;
    }
    openFolderIn(m_left, m_watch, &error);
    const int before = m_left->chain->depth();
    m_left->chain->selectFirstFolder();
    qInfo("self-check set up the transfer: left depth %d -> %d on %s, right on %s", before,
          m_left->chain->depth(),
          qUtf8Printable(QDir::toNativeSeparators(m_left->chain->deepestPath())),
          qUtf8Printable(QDir::toNativeSeparators(m_right->chain->deepestPath())));
}

bool MainWindow::dropBetweenPanes(const QString &from, const QString &to, bool copy) {
    if (from.isEmpty() || to.isEmpty() || folderKey(from) == folderKey(to)) {
        qWarning("self-check has two different folders to transfer between: %s -> %s",
                 qUtf8Printable(QDir::toNativeSeparators(from)),
                 qUtf8Printable(QDir::toNativeSeparators(to)));
        return false;
    }
    m_draggedFrom = QDir(from).filePath(QLatin1String(kDragProbe));
    m_draggedTo = QDir(to).filePath(QLatin1String(kDragProbe));
    if (!QFile::exists(m_draggedFrom)) {
        QFile file(m_draggedFrom);
        if (!file.open(QIODevice::WriteOnly | QIODevice::Truncate)) {
            qWarning("self-check could not create %s: %s",
                     qUtf8Printable(QDir::toNativeSeparators(m_draggedFrom)),
                     qUtf8Printable(file.errorString()));
            m_draggedFrom.clear();
            return false;
        }
        file.write("drag");
        file.close();
    }

    FileView *target = nullptr;
    for (Pane *pane : {m_left, m_right}) {
        if (folderKey(pane->chain->deepestPath()) == folderKey(to)) {
            target = pane->chain->deepestView();
        }
    }
    if (target == nullptr) {
        qWarning("self-check found no column showing %s to drop onto",
                 qUtf8Printable(QDir::toNativeSeparators(to)));
        return false;
    }
    checkBorderForSelfCheck(target, m_draggedFrom);
    QString outcome;
    const bool taken = postDrop(target, m_draggedFrom, copy, &outcome);
    /* This is the line the step exists for. A move that quietly came back a copy would look identical
     * in a status bar and identical in a log of handlers run, and the only thing that tells them apart
     * is whether the file is still where it started - so both paths are checked on disk, by name, in
     * the same sentence. */
    qInfo("self-check %s by drop from %s to %s: left depth=%d, right depth=%d, view answered %s, "
          "taken=%d, %s exists=%d, %s exists=%d",
          copy ? "copied" : "moved", qUtf8Printable(QDir::toNativeSeparators(from)),
          qUtf8Printable(QDir::toNativeSeparators(to)), m_left->chain->depth(),
          m_right->chain->depth(), qUtf8Printable(outcome), taken ? 1 : 0,
          qUtf8Printable(QDir::toNativeSeparators(m_draggedFrom)),
          QFile::exists(m_draggedFrom) ? 1 : 0,
          qUtf8Printable(QDir::toNativeSeparators(m_draggedTo)),
          QFile::exists(m_draggedTo) ? 1 : 0);
    return taken;
}

void MainWindow::moveAcrossPanesForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    reportDragPayload(m_right->chain->deepestView());
    if (!dropBetweenPanes(m_left->chain->deepestPath(), m_right->chain->deepestPath(), false)) {
        return;
    }
    /* A move leaves the file at the far end, and the next step's source has to be the file that
     * exists - not the path the last one was written at. */
    if (!QFile::exists(m_draggedFrom) && QFile::exists(m_draggedTo)) {
        m_draggedFrom = m_draggedTo;
    }
}

void MainWindow::copyBackAcrossPanesForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    if (!dropBetweenPanes(m_right->chain->deepestPath(), m_left->chain->deepestPath(), true)) {
        return;
    }
    /* A copy leaves a file at both ends, which is the evidence - and the extra one is what the cleanup
     * step is told about so it can remove it. */
}

/* The drop that goes nowhere: the item is dropped back into the folder already holding it. The claim
 * is in the line `handleDrop` writes for itself - one item "already there", none transferred, none
 * refused - because a guard that had not fired would have asked the core and been told SamePath, which
 * reads as an error about something a person did by accident. */
void MainWindow::dropInPlaceForSelfCheck() {
    if (!m_healthy || m_draggedFrom.isEmpty()) {
        qWarning("self-check had no file to drop back into its own folder");
        return;
    }
    const QString home = QFileInfo(m_draggedFrom).absolutePath();
    FileView *view = nullptr;
    for (Pane *pane : {m_left, m_right}) {
        if (folderKey(pane->chain->deepestPath()) == folderKey(home)) {
            view = pane->chain->deepestView();
        }
    }
    if (view == nullptr) {
        qWarning("self-check found no column showing %s",
                 qUtf8Printable(QDir::toNativeSeparators(home)));
        return;
    }
    QString outcome;
    const QString before = m_status->text();
    const bool taken = postDrop(view, m_draggedFrom, false, &outcome);
    /* The status line, before and after. `handleDrop` owns the sentence, so a line that changed is the
     * proof the handler ran at all - which is the thing this step was written to find: posted on its
     * own, with no drag-enter in front of it, the drop was answered by Qt's default and the step
     * reported success while nothing in this window had seen the event. */
    qInfo("self-check dropped %s back into its own folder: view answered %s, taken=%d, still there=%d, "
          "status was \"%s\" now \"%s\"",
          qUtf8Printable(QFileInfo(m_draggedFrom).fileName()), qUtf8Printable(outcome), taken ? 1 : 0,
          QFile::exists(m_draggedFrom) ? 1 : 0, qUtf8Printable(before),
          qUtf8Printable(m_status->text()));
}
