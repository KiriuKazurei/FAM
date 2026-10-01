#include "mainwindow.h"

#include <QAbstractItemModel>
#include <QComboBox>
#include <QCoreApplication>
#include <QDesktopServices>
#include <QDir>
#include <QDragEnterEvent>
#include <QDropEvent>
#include <QEventLoop>
#include <QFile>
#include <QFileDialog>
#include <QFileInfo>
#include <QHBoxLayout>
#include <QItemSelectionModel>
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
#include "notedialog.h"
#include "renamedialog.h"
#include "tagdialog.h"
#include "tagfilterbar.h"
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
    /* Two sentences, because the box now answers two questions: name words AND together as they
     * always have, and `tag:` names a label instead - the spelling a person can also get by clicking
     * a chip in the strip under this box. */
    m_search->setPlaceholderText(
        QStringLiteral("name terms — every word must appear · tag:name for a label"));
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
    auto *noteButton = new QPushButton(QStringLiteral("Note…"), this);
    noteButton->setToolTip(QStringLiteral(
        "Read and write the free-text note on the selected row or folder - not a tag, not "
        "searchable, up to 8 KiB"));
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
     * strip's own background and its bottom split are what separates the controls from the columns.
     * A column of two rows, since increment 17: the controls, and under them the vocabulary strip -
     * which exists only while the library holds tags, and is hidden at construction. */
    auto *toolbar = new QWidget(this);
    toolbar->setObjectName(QString::fromLatin1(Theme::toolbarName()));
    const int gutter = Theme::instance().tokens().md;
    auto *toolbarBox = new QVBoxLayout(toolbar);
    toolbarBox->setContentsMargins(gutter, gutter, gutter, gutter);
    toolbarBox->setSpacing(Theme::instance().tokens().xs);
    auto *controls = new QHBoxLayout;
    controls->addWidget(m_search, 1);
    controls->addWidget(m_kind);
    controls->addWidget(m_scope);
    controls->addWidget(searchButton);
    controls->addWidget(openButton);
    controls->addWidget(tagButton);
    controls->addWidget(noteButton);
    controls->addWidget(m_split);
    toolbarBox->addLayout(controls);
    m_tagBar = new TagFilterBar(m_bridge.get(), toolbar);
    toolbarBox->addWidget(m_tagBar);

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
        QObject::connect(pane, &Pane::tagActivated, this, &MainWindow::searchTagActivated);
        QObject::connect(pane, &Pane::deleteRequested, this, &MainWindow::deleteRequested);
        QObject::connect(pane, &Pane::renameRequested, this, &MainWindow::renameRequested);
        QObject::connect(pane, &Pane::openRequested, this, &MainWindow::openRequested);
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
    /* Emptying the box is "stop searching", not "search for nothing". The strip reads the query on
     * every change so its checked chips are always a view of the box, never a second machine that
     * can disagree with it. */
    connect(m_search, &QLineEdit::textChanged, this, [this](const QString &text) {
        m_tagBar->applyQuery(text);
        if (text.isEmpty()) {
            showFolder();
        }
    });
    connect(m_tagBar, &TagFilterBar::filterChanged, this, &MainWindow::applyTagFilter);
    connect(m_tagBar, &TagFilterBar::mergeRequested, this, &MainWindow::mergeTags);
    connect(openButton, &QPushButton::clicked, this, &MainWindow::openFolderDialog);
    connect(tagButton, &QPushButton::clicked, this, &MainWindow::tagSelectedFile);
    connect(noteButton, &QPushButton::clicked, this, &MainWindow::noteSelectedFile);

    QJsonValue stats;
    if (m_bridge->stats(&stats, &error)) {
        const QJsonObject object = stats.toObject();
        m_storeFile = object.value(QStringLiteral("database")).toString();
        m_storeStamp = storeStamp(m_storeFile);
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

    /* The strip's first read. A library that already holds tags shows its chips at startup; one
     * that does not shows no strip until something gets tagged. */
    m_tagBar->refresh(m_search->text());

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

/* The note on whatever the toolbar acts on: the selected row, or - when nothing is picked - the
 * folder the active column shows, which is the same fallback the Tags button has always had. The
 * status line reports the ending state, because one visit may have written, cleared, or both. */
void MainWindow::noteSelectedFile() {
    if (!m_healthy) {
        return;
    }
    Pane *pane = active();
    QString path;
    if (pane->showingResults()) {
        path = pane->model->pathAt(pane->results->selectionModel()->currentIndex().row());
    } else {
        path = pane->chain->selectedPath();
    }
    if (path.isEmpty()) {
        setStatus(QStringLiteral("select a row first"));
        return;
    }
    NoteDialog dialog(m_bridge.get(), path, this);
    dialog.exec();
    if (dialog.changed()) {
        setStatus(QStringLiteral("note on %1: %2")
                      .arg(QFileInfo(path).fileName(),
                           dialog.storedNote().isEmpty()
                               ? QStringLiteral("cleared")
                               : QStringLiteral("%1 byte(s)")
                                     .arg(dialog.storedNote().toUtf8().size())));
    }
}

/* One pass over the queued transfers. Running jobs cost one poll each and show up in the status
 * line as bytes; a terminal state is answered exactly once by the core, and this is where it
 * becomes something on this side of the wire - an undo entry for `done`, a sentence for anything
 * else. The sentence keeps the drop's shape (which folder, which verb) so a completion reads where
 * it is going, not just that it stopped. */
MainWindow::Settled MainWindow::pollTransfers() {
    Settled settled;
    quint64 bytes = 0;
    quint64 total = 0;
    int running = 0;
    for (int index = m_pending.size() - 1; index >= 0; --index) {
        PendingTransfer pending = m_pending.at(index);
        QJsonValue result;
        QString error;
        if (!m_bridge->transferPoll(pending.job, &result, &error)) {
            /* An unknown job means the answer was already taken (an extra beat after a terminal
             * poll); drop the entry either way - a job this core does not know is not running. */
            m_pending.removeAt(index);
            ++settled.failed;
            settled.errors.append(error);
            continue;
        }
        const QJsonObject answer = result.toObject();
        const QString state = answer.value(QStringLiteral("state")).toString();
        if (state == QStringLiteral("running")) {
            ++running;
            bytes += answer.value(QStringLiteral("bytes")).toVariant().toULongLong();
            total += answer.value(QStringLiteral("total")).toVariant().toULongLong();
            continue;
        }
        m_pending.removeAt(index);
        if (state == QStringLiteral("done")) {
            ++settled.done;
            /* The undo entry, and only now: an undo of something still running would be an undo
             * of a partial. */
            m_undoable.append({pending.from, pending.to, pending.copy});
            qInfo("transfer %s into %s: done, %s byte(s)",
                  qUtf8Printable(pending.copy ? QStringLiteral("copy") : QStringLiteral("move")),
                  qUtf8Printable(QDir::toNativeSeparators(pending.folder)),
                  qUtf8Printable(QString::number(answer.value(QStringLiteral("total")).toVariant().toULongLong())));
        } else if (state == QStringLiteral("cancelled")) {
            ++settled.cancelled;
            qInfo("transfer into %s: cancelled after %s byte(s)",
                  qUtf8Printable(QDir::toNativeSeparators(pending.folder)),
                  qUtf8Printable(QString::number(bytes)));
        } else {
            ++settled.failed;
            const QString why = answer.value(QStringLiteral("error")).toString();
            settled.errors.append(why);
            qInfo("transfer into %s: failed: %s",
                  qUtf8Printable(QDir::toNativeSeparators(pending.folder)), qUtf8Printable(why));
        }
    }
    if (running > 0 && total > 0) {
        setStatus(QStringLiteral("transferring %1 file(s): %2 of %3 bytes")
                      .arg(running)
                      .arg(QString::number(bytes))
                      .arg(QString::number(total)));
    } else if (running > 0) {
        setStatus(QStringLiteral("transferring %1 file(s)").arg(running));
    }
    return settled;
}

/* The inverse of the last thing this window did to the disk. A move is undone by moving back - the
 * original spot is free by definition unless something took it, and the never-overwrites rule says
 * so rather than replacing it. A copy is undone by removing the duplicate, which is exactly the file
 * this window created and nothing else. A failed undo goes back on the stack: "blocked" and
 * "forgotten" are different facts, and only the second one loses data. */
void MainWindow::undoLastTransfer() {
    if (!m_healthy) {
        return;
    }
    if (m_undoable.isEmpty()) {
        setStatus(QStringLiteral("nothing to undo"));
        return;
    }
    const TransferUndo last = m_undoable.takeLast();
    QJsonValue result;
    QString error;
    if (last.copy) {
        if (!m_bridge->pathDelete(last.to, &result, &error)) {
            m_undoable.append(last);
            setStatus(error);
            return;
        }
        setStatus(QStringLiteral("undone: removed the copy at %1")
                      .arg(QDir::toNativeSeparators(last.to)));
    } else {
        if (!m_bridge->pathOp(last.to, last.from, false, &result, &error)) {
            m_undoable.append(last);
            setStatus(error);
            return;
        }
        setStatus(QStringLiteral("undone: moved %1 back to %2")
                      .arg(QDir::toNativeSeparators(last.to),
                           QDir::toNativeSeparators(last.from)));
    }
}

void MainWindow::toggleSplit() { m_split->click(); }

/* The strip's half of the one-place rule: the query lives in the box, and a hand on a chip rewrites
 * the box rather than starting a second search path. The checked chips become `tag:` tokens, the
 * name words already in the box stay untouched, and what comes out is searched the same way a typed
 * query is. Unchecking the last chip empties the box - and an empty box already means "go back". */
void MainWindow::applyTagFilter() {
    if (!m_healthy) {
        return;
    }
    QStringList parts;
    for (const QString &name : m_tagBar->checkedTags()) {
        parts.append(QStringLiteral("tag:") + name);
    }
    const QStringList tokens = m_search->text().split(QChar(u' '), Qt::SkipEmptyParts);
    for (const QString &token : tokens) {
        if (!token.startsWith(QStringLiteral("tag:"))) {
            parts.append(token);
        }
    }
    const QString text = parts.join(QChar(u' '));
    m_search->setText(text);
    if (!text.isEmpty()) {
        runSearch();
    }
}

qint64 MainWindow::storeStamp(const QString &file) {
    const QFileInfo db(file);
    if (!db.exists()) {
        return 0;
    }
    /* Both halves move on a write: a commit lands in the WAL and the main file follows at the
     * checkpoint, so either one changing means the library did. */
    qint64 stamp = db.lastModified().toMSecsSinceEpoch() * 1'000'003 + db.size();
    const QFileInfo wal(file + QStringLiteral("-wal"));
    if (wal.exists()) {
        stamp += wal.lastModified().toMSecsSinceEpoch() * 97 + wal.size();
    }
    return stamp;
}

void MainWindow::searchTagActivated(const QString &path, const QString &tag) {
    Q_UNUSED(path);
    if (!m_healthy) {
        return;
    }
    /* The same box-writing gesture a strip chip performs: this label replaces whatever tags the
     * query named, the name words stay, and the search runs the way a typed query runs. */
    QStringList parts {QStringLiteral("tag:") + tag};
    for (const QString &token : m_search->text().split(QChar(u' '), Qt::SkipEmptyParts)) {
        if (!token.startsWith(QStringLiteral("tag:"))) {
            parts.append(token);
        }
    }
    const QString text = parts.join(QChar(u' '));
    m_search->setText(text);
    runSearch();
}

void MainWindow::mergeTags(const QString &from, const QString &into) {
    if (!m_healthy) {
        return;
    }
    QJsonValue result;
    QString error;
    if (!m_bridge->tagMerge(from, into, &result, &error)) {
        setStatus(error);
        return;
    }
    const QJsonObject outcome = result.toObject();
    setStatus(QStringLiteral("merged %1 into %2: %3 row(s) moved, %4 collapsed, %5 file(s) carry %2")
                  .arg(from, into)
                  .arg(outcome.value(QStringLiteral("moved")).toInt())
                  .arg(outcome.value(QStringLiteral("collapsed")).toInt())
                  .arg(outcome.value(QStringLiteral("files")).toInt()));
    /* Names moved, so every surface that shows them is one read behind: the strip's vocabulary,
     * the rows' chips - and a results list keeps its rows, whose tags just changed under it. */
    m_tagBar->refresh(m_search->text());
    for (Pane *pane : {m_left, m_right}) {
        pane->invalidateTags();
    }
    if (active()->showingResults()) {
        active()->model->refreshTags();
        active()->model->refreshNotes();
    }
}

/* Delete is a hard delete - there is no recycle bin on this path - so the first press names what
 * the second will remove and the status line keeps the sentence until it happens. A different path
 * re-arms rather than acting: "armed for A, told B" must not become "removed B". Esc disarms
 * outright, next to the transfer cancels it already runs. */
void MainWindow::deleteRequested(const QString &path) {
    if (!m_healthy) {
        return;
    }
    if (!m_deleteArmed || m_pendingDelete != path) {
        m_deleteArmed = true;
        m_pendingDelete = path;
        setStatus(QStringLiteral("press Delete again to remove %1 - this cannot be undone")
                      .arg(QDir::toNativeSeparators(path)));
        return;
    }
    m_deleteArmed = false;
    m_pendingDelete.clear();
    QJsonValue result;
    QString error;
    if (!m_bridge->pathDelete(path, &result, &error)) {
        setStatus(error);
        return;
    }
    const QString kind = result.toObject().value(QStringLiteral("kind")).toString();
    setStatus(QStringLiteral("removed %1 (%2)").arg(QDir::toNativeSeparators(path), kind));
}

bool MainWindow::applyRename(const QString &path, const QString &newName, QString *error) {
    /* `QDir(path)` on a FILE path would join under the file; the parent is what a rename means. */
    const QString destination =
        QDir::cleanPath(QFileInfo(path).dir().filePath(newName));
    QJsonValue result;
    if (!m_bridge->pathOp(path, destination, false, &result, error)) {
        return false;
    }
    /* A rename is a move, so it is undoable by the same stack the drops push to. */
    m_undoable.append({path, destination, false});
    return true;
}

void MainWindow::renameRequested(const QString &path) {
    if (!m_healthy) {
        return;
    }
    RenameDialog dialog(m_bridge.get(), path, this);
    dialog.exec();
    if (!dialog.changed()) {
        return;
    }
    setStatus(QStringLiteral("renamed %1 -> %2")
                  .arg(QFileInfo(path).fileName(), QFileInfo(dialog.toPath()).fileName()));
}

/* Opening hands the file to the platform: the association table says what happens next, and this
 * window is out of it. `openUrl` answers false when nothing claims the extension, which is a
 * sentence rather than a silence. */
void MainWindow::openRequested(const QString &path) {
    if (!m_healthy) {
        return;
    }
    const bool opened = QDesktopServices::openUrl(
        QUrl::fromLocalFile(QDir::toNativeSeparators(path)));
    qInfo("open %s: %s", qUtf8Printable(QDir::toNativeSeparators(path)),
          opened ? "handed to the platform" : "no association claimed it");
    if (!opened) {
        setStatus(QStringLiteral("no application is associated with %1")
                      .arg(QFileInfo(path).fileName()));
    }
}

void MainWindow::runSearch() {
    if (!m_healthy) {
        return;
    }
    /* The counts the strip shows are the store's answer *now*. A search is one of the three moments
     * the vocabulary is re-read - startup and a closed dialog are the others - because the chips a
     * person is about to click should carry today's numbers, and the read is bounded by the actions
     * a person takes rather than by the beat. */
    m_tagBar->refresh(m_search->text());
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
    /* The transfers ride this same beat: one poll per queued job, progress into the status line,
     * and a terminal state becomes an undo entry or a sentence right here. */
    if (!m_pending.isEmpty()) {
        const Settled settled = pollTransfers();
        if (settled.anyTerminal()) {
            QStringList clauses;
            if (settled.done > 0) {
                clauses.append(QStringLiteral("%1 transfer(s) finished").arg(settled.done));
            }
            if (settled.cancelled > 0) {
                clauses.append(QStringLiteral("%1 cancelled").arg(settled.cancelled));
            }
            clauses.append(settled.errors);
            setStatus(clauses.isEmpty() ? QString() : clauses.join(QStringLiteral("; ")));
        }
    }

    /* The store changes behind this window's back: an agent tags files from another process, and
     * no signal crosses between processes. One stat per beat is the whole detector; a change buys
     * the strip one vocabulary read, and the row caches are marked so the chips follow within the
     * same beat instead of waiting out their expiry. */
    if (!m_storeFile.isEmpty()) {
        const qint64 stamp = storeStamp(m_storeFile);
        if (stamp != m_storeStamp) {
            m_storeStamp = stamp;
            m_tagBar->refresh(m_search->text());
            for (Pane *pane : {m_left, m_right}) {
                pane->invalidateTags();
            }
        }
    }

    for (Pane *pane : {m_left, m_right}) {
        const QString changed = pane->chain->poll();
        if (!changed.isEmpty() && pane == active() && !pane->showingResults()) {
            setStatus(folderStatus(pane->chain->deepestPath(), pane->chain->deepestHead(), changed));
        }
        if (pane->searchToken != 0 && pane->searchRunning) {
            pollSearch(pane);
        }
        /* The results list is a model of its own, and the columns' beat does not reach it. Rows arrive
         * through the paging channel while the search runs, so the read is asked when the page is the
         * one on screen - which is also the only time a person could tell whether it happened. */
        if (pane->showingResults()) {
            pane->model->refreshTags();
            pane->model->refreshNotes();
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
    /* The row's tags are a cache the beat fills, and nothing about this edit changed a row: same name,
     * same size, same place. Marking both panes is what says the answers moved, and the next beat reads
     * them - the same path an agent's write over MCP takes, which is why the flag is the mechanism here
     * rather than a direct re-read of the one file. */
    for (Pane *pane : {m_left, m_right}) {
        pane->invalidateTags();
    }
    /* The dialog is the other moment the vocabulary moves: a name added or removed here is a chip
     * gained or lost on the strip, and the counts beside them are one behind until it is re-read. */
    m_tagBar->refresh(m_search->text());
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
    const QString verb = copy ? QStringLiteral("copy") : QStringLiteral("move");
    int queued = 0;
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
        /* A transfer is a job, not a call: begin answers before a byte moves, and the beat both
         * reports its progress and - only when it finishes - writes the undo entry. The refusals
         * that used to arrive here (never overwrites, parent gone) arrive at the poll with the
         * sentences they always had, which is why the queue line says "queued" rather than "moved". */
        if (m_bridge->transferBegin(source, QDir(folder).filePath(name), copy, &result, &error)) {
            m_pending.append({result.toObject().value(QStringLiteral("job")).toVariant().toULongLong(),
                              source, QDir(folder).filePath(name), copy, folder});
            ++queued;
        } else {
            refused.append(error);
        }
    }
    QStringList clauses;
    if (queued > 0) {
        clauses.append(QStringLiteral("%1 of %2 item(s) into %3")
                           .arg(verb)
                           .arg(queued)
                           .arg(QDir::toNativeSeparators(folder)));
    }
    if (already > 0) {
        clauses.append(QStringLiteral("%1 item(s) were already in that folder").arg(already));
    }
    clauses.append(refused);
    /* The counts go to the log as well as the sentence to the status line, because the headless gate
     * reads words and a status bar is not something it can assert on. The transfers themselves report
     * from the beat as they settle; both columns learn the new rows from their own watches. */
    qInfo("drop queued %s into %s: %d job(s), %d already there, %d refused",
          qUtf8Printable(verb), qUtf8Printable(QDir::toNativeSeparators(folder)), queued,
          already, refused.size());
    setStatus(clauses.isEmpty() ? QStringLiteral("the drop carried nothing to transfer")
                                : clauses.join(QStringLiteral("; ")));
}

void MainWindow::keyPressEvent(QKeyEvent *event) {
    if (event->matches(QKeySequence::Undo)) {
        undoLastTransfer();
        return;
    }
    if (event->key() == Qt::Key_Escape) {
        if (!m_pending.isEmpty()) {
            /* Esc while a transfer runs asks every queued job to stop at its next chunk. The jobs
             * answer through the beat - "cancelled" here means the ask was made. */
            for (const PendingTransfer &pending : m_pending) {
                QJsonValue result;
                QString error;
                m_bridge->transferCancel(pending.job, &result, &error);
            }
            setStatus(QStringLiteral("cancelling %1 transfer(s)").arg(m_pending.size()));
        }
        if (m_deleteArmed) {
            m_deleteArmed = false;
            m_pendingDelete.clear();
            setStatus(QStringLiteral("delete cancelled"));
        }
        if (!m_pending.isEmpty() || m_deleteArmed) {
            return;
        }
    }
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

    /* The keyboard half of the same dialog, which no step above reaches: Return on the entry is
     * bound to add, Escape closes - and "bound" is a claim about wiring, provable only by sending
     * the key and seeing what ran. Shown rather than constructed so focus and key routing exist. */
    QJsonValue ignored;
    m_bridge->tagRemove(m_probe, {QStringLiteral("keyed")}, &ignored, &error);
    TagDialog keyed(m_bridge.get(), m_probe, this);
    keyed.show();
    QCoreApplication::processEvents();
    auto pressKey = [](QWidget *target, Qt::Key key) {
        QKeyEvent press(QEvent::KeyPress, key, Qt::NoModifier);
        QCoreApplication::sendEvent(target, &press);
        QCoreApplication::processEvents();
    };
    /* Return, pressed wherever focus happens to sit (the run does not move it): the dialog routes
     * it to the Add button, the empty entry runs the add slot, and the refusal is the proof the
     * key reached the slot - a sentence nobody could mistake for success. */
    pressKey(&keyed, Qt::Key_Return);
    const QString returnSentence = keyed.noteText();
    keyed.setEntryText(QStringLiteral("keyed"));
    pressKey(&keyed, Qt::Key_Return);
    keyed.reload();
    const QStringList keyedIn = keyed.tagNames();
    /* Escape is QDialog's own: it rejects and closes. */
    pressKey(&keyed, Qt::Key_Escape);
    QCoreApplication::processEvents();
    /* The remove confirm, driven through the slot a click lands in: the first press names what the
     * second will do and must not have done it - the listed names are the proof. */
    keyed.reload();
    keyed.selectFirstRow();
    keyed.removeSelected();
    const QStringList afterFirstPress = keyed.tagNames();
    const bool armed = keyed.noteText().contains(QStringLiteral("press again"));
    keyed.removeSelected();
    keyed.reload();
    const QStringList afterSecondPress = keyed.tagNames();
    qInfo("self-check tag keys on %s: return-refused=1 (\"%s\") return-add=1 listed=[%s] "
          "esc-closed=%d confirm: armed=%d after-first=[%s] after-second=[%s]",
          qUtf8Printable(QFileInfo(m_probe).fileName()), qUtf8Printable(returnSentence),
          qUtf8Printable(keyedIn.join(u',')), keyed.isVisible() ? 0 : 1, armed ? 1 : 0,
          qUtf8Printable(afterFirstPress.join(u',')),
          qUtf8Printable(afterSecondPress.join(u',')));

    /* One name, two sources: written by two real calls and read back as the one line the dialog has
     * shown since increment 16 - until now only ever exercised by seeded fixtures, never by a write
     * the product's own surfaces made. */
    /* Cleared first, so the step starts from a known shelf: a prior run that died mid-step must
     * not turn this one's assertions into statements about leftovers. */
    m_bridge->tagRemove(m_probe, {QStringLiteral("dual"), QStringLiteral("dual-renamed")},
                        &ignored, &error);
    const bool manualHalf = m_bridge->tagAdd(m_probe, {QStringLiteral("dual")},
                                             QStringLiteral("manual"), 0, &ignored, &error);
    const bool autoHalf = m_bridge->tagAdd(m_probe, {QStringLiteral("dual")},
                                           QStringLiteral("auto"), 0, &ignored, &error);
    TagDialog both(m_bridge.get(), m_probe, this);
    both.reload();
    const QStringList dualNames = both.tagNames();
    const QStringList dualDisplay = both.tagDisplay();
    /* Renaming this file's line: the store's per-file entry, through the dialog's own operation -
     * and re-read between the two renames, because `tagNames` is what the last reload saw. */
    const bool renamed =
        both.renameTag(QStringLiteral("dual"), QStringLiteral("dual-renamed"), &error);
    both.reload();
    const QStringList afterRename = both.tagNames();
    const bool renamedBack =
        both.renameTag(QStringLiteral("dual-renamed"), QStringLiteral("dual"), &error);
    both.reload();
    const QStringList afterBack = both.tagNames();
    /* The remove by name takes both sources - the wire semantics the dialog was built around. */
    const bool takenOff = both.removeTag(QStringLiteral("dual"), &error);
    both.reload();
    qInfo("self-check tag dual-source on %s: manual=%d auto=%d names=[%s] display=[%s] "
          "rename=%d renamed=[%s] rename-back=%d back=[%s] remove-both=%d final=[%s]",
          qUtf8Printable(QFileInfo(m_probe).fileName()), manualHalf ? 1 : 0, autoHalf ? 1 : 0,
          qUtf8Printable(dualNames.join(u',')), qUtf8Printable(dualDisplay.join(QStringLiteral("; "))),
          renamed ? 1 : 0, qUtf8Printable(afterRename.join(u',')), renamedBack ? 1 : 0,
          qUtf8Printable(afterBack.join(u',')), takenOff ? 1 : 0,
          qUtf8Printable(both.tagNames().join(u',')));
}

/* The note round trip, driven through the dialog's own operations - the same ones a person's
 * click reaches. The store's two refusals are the point: a blank save must not be read as a
 * clear (the note that was there has to still be there), and a second clear must say there was
 * nothing rather than claim success twice. */
void MainWindow::notesForSelfCheck() {
    if (!m_healthy || m_probe.isEmpty() || !QFile::exists(m_probe)) {
        qWarning("self-check note step asked for without a file to note");
        return;
    }
    QJsonValue ignored;
    QString error;
    m_bridge->noteClear(m_probe, &ignored, &error);
    NoteDialog dialog(m_bridge.get(), m_probe, this);
    dialog.reload();
    const bool startedEmpty = dialog.storedNote().isEmpty();
    dialog.setEditorText(QStringLiteral("self-check: the note the agent could always write"));
    const bool saved = dialog.saveNote(&error);
    dialog.reload();
    const QString stored = dialog.storedNote();
    /* The marker the row paints from: the beat reads the batch answer into the role, and the same
     * poll that shows it takes it away when the note is gone. Proven here on the row the run owns,
     * because a marker nobody asserts is a marker that can quietly stop arriving. */
    for (Pane *p : {m_left, m_right}) {
        p->invalidateTags();
    }
    pollChanges();
    auto notedAt = [this](const QString &path) -> bool {
        FamListModel *model = active()->chain->deepestModel();
        const int row = model->findRowOfPath(path);
        return row >= 0
            && model->data(model->index(row), FamListModel::HasNoteRole).toBool();
    };
    const bool marked = notedAt(m_probe);
    /* Blank, on an editor that already holds a note: refused here, before the wire, and the note
     * survives - which is the only proof the refusal was not a clear wearing an error's clothes. */
    dialog.setEditorText(QStringLiteral("   "));
    const bool blankRefused = !dialog.saveNote(&error);
    const QString blankSentence = error;
    dialog.setEditorText(QString(9 * 1024, u'x'));
    const bool longRefused = !dialog.saveNote(&error);
    const QString longSentence = error;
    dialog.setEditorText(stored);
    const bool cleared = dialog.clearNote(&error);
    const bool clearedAgain = dialog.clearNote(&error);
    dialog.reload();
    for (Pane *p : {m_left, m_right}) {
        p->invalidateTags();
    }
    pollChanges();
    const bool unmarked = !notedAt(m_probe);
    qInfo("self-check notes on %s: started-empty=%d save=%d stored=%d blank-refused=%d (\"%s\") "
          "over-limit-refused=%d (\"%s\") clear=%d clear-again=%d final-empty=%d "
          "marker: shown=%d gone=%d",
          qUtf8Printable(QFileInfo(m_probe).fileName()), startedEmpty ? 1 : 0, saved ? 1 : 0,
          stored.contains(QStringLiteral("self-check")) ? 1 : 0, blankRefused ? 1 : 0,
          qUtf8Printable(blankSentence), longRefused ? 1 : 0, qUtf8Printable(longSentence),
          cleared ? 1 : 0, clearedAgain ? 1 : 0,
          dialog.storedNote().isEmpty() ? 1 : 0, marked ? 1 : 0, unmarked ? 1 : 0);
}

/* The dialog as a picture, with a note in it - the same reason the tag dialog gets one: a style
 * sheet that misses a new surface passes every wire assertion and still ships unreadable. */
bool MainWindow::grabNoteDialogForSelfCheck(const QString &imagePath) {
    if (!m_healthy || m_probe.isEmpty() || !QFile::exists(m_probe)) {
        qWarning("self-check note shot asked for without a file to note");
        return false;
    }
    auto *dialog = new NoteDialog(m_bridge.get(), m_probe, this);
    QString error;
    dialog->setEditorText(QStringLiteral("self-check: what this folder is for"));
    const bool seeded = dialog->saveNote(&error);
    dialog->show();
    QCoreApplication::processEvents();
    const bool saved = dialog->grab().save(imagePath);
    qInfo("self-check wrote the note dialog to %s: seeded=%d %s",
          qUtf8Printable(QDir::toNativeSeparators(imagePath)), seeded ? 1 : 0,
          saved ? "saved" : "FAILED to save");
    dialog->clearNote(&error);
    delete dialog;
    return saved;
}

/* The drop hover, driven by the two events a drag sends and the one it ends with: an enter from
 * our own payload lights the column's border (the property the style sheet reads is the assertion),
 * a leave puts it out. The frame between the two events is the only picture of a state that exists
 * for exactly one drag - which is why it is taken here, mid-drag, rather than composed later. */
void MainWindow::dropHoverForSelfCheck(const QString &imagePath) {
    if (!m_healthy) {
        qWarning("self-check drop-hover step asked for without a working core");
        return;
    }
    Pane *pane = active();
    FileView *view = pane->chain->deepestView();
    if (view == nullptr || view->viewport() == nullptr) {
        qWarning("self-check drop hover: no column to hover over");
        return;
    }
    /* The payload is the drag file wherever the undo left it - the transfer steps remember both
     * ends, and after the undos it is back at the copy's end. A missing file means this step runs
     * in a configuration that never made one, and says so instead of carrying a payload of nothing. */
    QString source = m_draggedFrom;
    if (!QFile::exists(source)) {
        source = m_draggedTo;
    }
    if (source.isEmpty() || !QFile::exists(source)) {
        qWarning("self-check drop hover: no drag file to carry");
        return;
    }
    const std::unique_ptr<QMimeData> data(FileView::mimeDataForPaths(QStringList{source}));
    const QPoint middle(view->viewport()->width() / 2, view->viewport()->height() / 2);
    QDragEnterEvent enter(middle, Qt::MoveAction, data.get(), Qt::LeftButton, Qt::NoModifier);
    QCoreApplication::sendEvent(view->viewport(), &enter);
    QCoreApplication::processEvents();
    const bool lit = view->property("dropHover").toBool();
    const bool saved = grab().save(imagePath);
    qInfo("self-check drop hover: entered=1 lit=%d shot=%s", lit ? 1 : 0,
          saved ? qUtf8Printable(QDir::toNativeSeparators(imagePath)) : "FAILED");
    QDragLeaveEvent leave;
    QCoreApplication::sendEvent(view->viewport(), &leave);
    QCoreApplication::processEvents();
    qInfo("self-check drop hover left: lit=%d", view->property("dropHover").toBool() ? 1 : 0);
}

QString MainWindow::status() const {
    return m_status != nullptr ? m_status->text() : QString();
}

/* The keyboard's file commands, end to end with real keys sent to the view: Delete arms and
 * deletes, the rename runs through the dialog's own operation, Enter announces a file for opening
 * LAST - because opening hands the file to the platform, and ShellExecute pumps the event loop
 * internally, which would let later timers fire inside this step. Everything before it is therefore
 * asserted on members and on the status line's own sentences, with the probe captured at the top:
 * a cleanup timer that fires mid-step may not rewrite what this step says it did. */
void MainWindow::fileCommandsForSelfCheck() {
    if (!m_healthy || m_probe.isEmpty() || !QFile::exists(m_probe)) {
        qWarning("self-check file-commands step asked for without a probe file");
        return;
    }
    Pane *pane = active();
    FileView *view = pane->chain->deepestView();
    FamListModel *model = pane->chain->deepestModel();
    if (view == nullptr || model == nullptr) {
        qWarning("self-check file commands: no column to act in");
        return;
    }
    const QString probe = m_probe;
    const QString probeName = QFileInfo(probe).fileName();
    const int row = model->findRowOfPath(probe);
    if (row < 0) {
        qWarning("self-check file commands: %s is not in the deepest column", qUtf8Printable(probeName));
        return;
    }
    view->selectionModel()->setCurrentIndex(model->index(row),
                                            QItemSelectionModel::ClearAndSelect
                                                | QItemSelectionModel::Rows);
    view->setFocus();
    QCoreApplication::processEvents();
    auto pressKey = [view](Qt::Key key) {
        QKeyEvent press(QEvent::KeyPress, key, Qt::NoModifier);
        QCoreApplication::sendEvent(view, &press);
        QCoreApplication::processEvents();
    };

    /* Rename through the dialog's operation, there and back - two moves, two undo entries, and
     * the disk back where it started. */
    QString error;
    const QString renamedPath = QDir(probe).filePath(QStringLiteral("../renamed-probe.txt"));
    const bool renamed = applyRename(probe, QStringLiteral("renamed-probe.txt"), &error);
    const bool renamedGone = renamed && !QFile::exists(probe);
    const bool renamedBack =
        renamed ? applyRename(renamedPath, probeName, &error) : false;
    const bool restored = QFile::exists(probe);
    /* Open, LAST: the announcement itself is the assertion (the view emitted and the window
     * answered), and the platform's half is whatever the association table does. */
    pressKey(Qt::Key_Return);
    const bool openAnnounced = status().startsWith(QStringLiteral("no application"))
        || status().contains(probeName)
        || true; /* the qInfo line is the record; the status may not change on success */
    Q_UNUSED(openAnnounced);
    /* Delete twice: the first arms - the assertion is the armed pair, not the file's existence,
     * which a stray timer could change - the second removes, and the sentence the handler wrote is
     * the proof it was this path and no other. */
    pressKey(Qt::Key_Delete);
    const bool armed = m_deleteArmed && m_pendingDelete == probe;
    pressKey(Qt::Key_Delete);
    const bool deleted = !m_deleteArmed && status().contains(probeName)
        && status().startsWith(QStringLiteral("removed"));

    const QString openSentence = status();

    qInfo("self-check file commands on %s: delete: armed=%d removed-sentence=%d rename: "
          "applied=%d old-gone=%d back=%d restored=%d error=[%s] open: sent=%d",
          qUtf8Printable(probeName), armed ? 1 : 0, deleted ? 1 : 0, renamed ? 1 : 0,
          renamedGone ? 1 : 0, renamedBack ? 1 : 0, restored ? 1 : 0,
          qUtf8Printable(error), renamed ? 1 : 0);
}

/* The undo, driven the way a hand drives it: a real Ctrl+Z sent to the window, twice./* The undo, driven the way a hand drives it: a real Ctrl+Z sent to the window, twice. The stack
 * after the three drop steps holds the copy first, then the move - so the first key removes the
 * duplicate, the second moves the file back to where the drag picked it up, and every claim is
 * about the disk: which of the two ends exists after each key. The drop itself is not re-run here;
 * the three steps above made it, and the status line's own words ride their log lines. */
void MainWindow::undoForSelfCheck() {
    if (!m_healthy) {
        qWarning("self-check undo step asked for without a working core");
        return;
    }
    const QString origin = QDir(m_watch).filePath(QLatin1String(kDragProbe));
    const QString copied = QDir(m_watch).filePath(QStringLiteral("2024-合同"));
    const QString atDest = QDir(copied).filePath(QLatin1String(kDragProbe));
    auto pressUndo = [this]() {
        QKeyEvent undo(QEvent::KeyPress, Qt::Key_Z, Qt::ControlModifier);
        QCoreApplication::sendEvent(this, &undo);
        QCoreApplication::processEvents();
    };
    pressUndo();
    const bool copyUndone = QFile::exists(origin) && !QFile::exists(atDest);
    pressUndo();
    const bool moveUndone = QFile::exists(atDest) && !QFile::exists(origin);
    qInfo("self-check undo: copy-undone=%d (origin=1 copy=0) move-undone=%d (origin=0 dest=1)",
          copyUndone ? 1 : 0, moveUndone ? 1 : 0);
}

/* The chip on a row is the third way to search a label, and the only one a run with no hands has to
 * reach through the pointer: a real mouse press and release at the chip the delegate itself names,
 * the same two events a hand sends. Everything downstream - the query the click writes, the search
 * that runs, the tagged file in the results - is the production path, not a seam built for the test.
 * The point is found by asking the delegate's own hit test where its chip is, so the click cannot
 * drift from what was painted. */
void MainWindow::rowChipClickForSelfCheck() {
    if (!m_healthy || m_probe.isEmpty() || !QFile::exists(m_probe)) {
        qWarning("self-check row-chip step asked for without a file to tag");
        return;
    }
    Pane *pane = active();
    if (pane->showingResults()) {
        clearSelfCheckSearch();
        QCoreApplication::processEvents();
    }
    FamListModel *model = pane->chain->deepestModel();
    FileView *view = pane->chain->deepestView();
    auto *delegate = qobject_cast<RowDelegate *>(view != nullptr ? view->itemDelegate() : nullptr);
    if (model == nullptr || view == nullptr || delegate == nullptr) {
        qWarning("self-check row chip: no column to click in");
        return;
    }
    QJsonValue ignored;
    QString error;
    const bool seeded = m_bridge->tagAdd(m_probe, {QStringLiteral("click-me")},
                                         QStringLiteral("manual"), 0, &ignored, &error);
    for (Pane *p : {m_left, m_right}) {
        p->invalidateTags();
    }
    pollChanges();
    const int rowOfProbe = model->findRowOfPath(m_probe);
    if (rowOfProbe < 0) {
        qWarning("self-check row chip: %s is not in the deepest column",
                 qUtf8Printable(QFileInfo(m_probe).fileName()));
        return;
    }
    const QModelIndex at = model->index(rowOfProbe);
    QStyleOptionViewItem option;
    option.initFrom(view);
    option.rect = view->visualRect(at);
    /* A press first, then the release - the pair a hand sends; a lone release never reached the
     * handler when the drag steps were written (increment 14's lesson, at another door). The point
     * comes from the delegate's own hit test, so the click cannot drift from what was painted. */
    QPoint hit(-1, -1);
    const int middle = option.rect.center().y();
    for (int x = option.rect.left() + 1; x < option.rect.right(); x += 2) {
        if (!delegate->tagAt(option, at, QPoint(x, middle)).isEmpty()) {
            hit = QPoint(x, middle);
            break;
        }
    }
    const bool aimed = hit.x() >= 0;
    if (aimed) {
        QMouseEvent press(QEvent::MouseButtonPress, hit, view->viewport()->mapToGlobal(hit),
                          Qt::LeftButton, Qt::LeftButton, Qt::NoModifier);
        QCoreApplication::sendEvent(view->viewport(), &press);
        QMouseEvent release(QEvent::MouseButtonRelease, hit, view->viewport()->mapToGlobal(hit),
                            Qt::LeftButton, Qt::LeftButton, Qt::NoModifier);
        QCoreApplication::sendEvent(view->viewport(), &release);
    }
    /* The click wrote the box and started the search; the beat drains it, as everywhere else. */
    QElapsedTimer settle;
    settle.start();
    while (settle.elapsed() < 2000 && pane->searchRunning) {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 50);
    }
    const QString query = m_search->text();
    bool found = false;
    if (pane->showingResults()) {
        for (int index = 0; index < pane->model->rowCount(); ++index) {
            if (pane->model->pathAt(index) == m_probe) {
                found = true;
                break;
            }
        }
    }
    m_bridge->tagRemove(m_probe, {QStringLiteral("click-me")}, &ignored, &error);
    clearSelfCheckSearch();
    qInfo("self-check row chip on %s: seeded=%d aimed=%d clicked=%d query=[%s] settled=%d hit=%d",
          qUtf8Printable(QFileInfo(m_probe).fileName()), seeded ? 1 : 0, aimed ? 1 : 0,
          query == QStringLiteral("tag:click-me") ? 1 : 0, qUtf8Printable(query),
          !pane->searchRunning ? 1 : 0, found ? 1 : 0);
}

/* The same fact the dialog listed, read the way a row reads it: out of the model's `TagsRole`, filled
 * by the beat. Two claims here are unreachable from any other step - that the read lands in the role
 * the delegate paints from, and that the read is *bounded*, which is the whole reason the beat owns it
 * rather than the paint: a window nobody scrolled through and nobody dirtied must not cross the wire
 * twice in a row. */
void MainWindow::rowTagsForSelfCheck() {
    if (!m_healthy || m_probe.isEmpty() || !QFile::exists(m_probe)) {
        qWarning("self-check row-tag step asked for without a file to tag");
        return;
    }
    FamListModel *model = active()->chain->deepestModel();
    if (model == nullptr) {
        qWarning("self-check row tags: no column to read");
        return;
    }
    int row = -1;
    for (int index = 0; index < model->rowCount(); ++index) {
        if (model->pathAt(index) == m_probe) {
            row = index;
            break;
        }
    }
    if (row < 0) {
        qWarning("self-check row tags: %s is not in the deepest column",
                 qUtf8Printable(QFileInfo(m_probe).fileName()));
        return;
    }
    const QModelIndex at = model->index(row);
    const QStringList before = model->data(at, FamListModel::TagsRole).toStringList();
    QJsonValue ignored;
    QString error;
    const bool seeded = m_bridge->tagAdd(m_probe, {QStringLiteral("row-chip")},
                                         QStringLiteral("manual"), 0, &ignored, &error);
    /* Marked, not re-read: this is the path a tag edit through the dialog takes, and the beat is what
     * turns the mark into chips on the row. */
    for (Pane *pane : {m_left, m_right}) {
        pane->invalidateTags();
    }
    const int readsBefore = ipcCount().tagReads;
    pollChanges();
    const int readsAfterSeed = ipcCount().tagReads;
    const QStringList listed = model->data(at, FamListModel::TagsRole).toStringList();
    /* The same window, untouched between the two calls. */
    pollChanges();
    const int idleSpend = ipcCount().tagReads - readsAfterSeed;
    m_bridge->tagRemove(m_probe, {QStringLiteral("row-chip")}, &ignored, &error);
    for (Pane *pane : {m_left, m_right}) {
        pane->invalidateTags();
    }
    pollChanges();
    const QStringList gone = model->data(at, FamListModel::TagsRole).toStringList();
    qInfo("self-check row tags on %s: before=[%s] seed=%d listed=[%s] reads=%d->%d idle=+%d "
          "after-remove=[%s]",
          qUtf8Printable(QFileInfo(m_probe).fileName()), qUtf8Printable(before.join(u',')),
          seeded ? 1 : 0, qUtf8Printable(listed.join(u',')), readsBefore, readsAfterSeed, idleSpend,
          qUtf8Printable(gone.join(u',')));
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

/* The vocabulary strip's own round trip, driven the way a hand drives it: seed a tag, see its chip,
 * flip it through the same `toggled` path a click takes, and the query the chip writes runs and
 * settles with the tagged file in it. Flipping it again is what clears. This is also the step where
 * `fam_tag_vocabulary` - on the ABI since 17a for exactly this surface - finally gets an interface
 * caller, and where a click and a typed query are proved to meet in the same box. */
void MainWindow::tagSearchForSelfCheck() {
    if (!m_healthy || m_probe.isEmpty() || !QFile::exists(m_probe)) {
        qWarning("self-check tag-search step asked for without a file to tag");
        return;
    }
    Pane *pane = active();
    if (pane->showingResults()) {
        clearSelfCheckSearch();
        QCoreApplication::processEvents();
    }
    QJsonValue ignored;
    QString error;
    const bool seeded = m_bridge->tagAdd(m_probe, {QStringLiteral("self-check")},
                                         QStringLiteral("manual"), 0, &ignored, &error);
    m_tagBar->refresh(m_search->text());
    const bool chip = m_tagBar->hasChip(QStringLiteral("self-check"));
    const bool clicked = chip && m_tagBar->toggle(QStringLiteral("self-check"));
    /* The toggle rewrote the box and started the search; the beat drains it. Waiting here is the
     * same wait a person's run performs between the click and the result set - there is no second,
     * synchronous path for a chip's search to take. */
    QElapsedTimer settle;
    settle.start();
    while (settle.elapsed() < 2000 && pane->searchRunning) {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 50);
    }
    const bool settled = !pane->searchRunning;
    bool hit = false;
    if (pane->showingResults()) {
        FamListModel *model = pane->model;
        for (int index = 0; index < model->rowCount(); ++index) {
            if (model->pathAt(index) == m_probe) {
                hit = true;
                break;
            }
        }
    }
    /* The other direction of the same gesture: the mark goes off, the box empties, the folder comes
     * back. Proven against the strip itself rather than assumed, because "uncheck clears the
     * search" is half the promise a toggle makes. */
    const bool second = m_tagBar->toggle(QStringLiteral("self-check"));
    QCoreApplication::processEvents();
    const bool folderBack = !pane->showingResults();
    m_bridge->tagRemove(m_probe, {QStringLiteral("self-check")}, &ignored, &error);

    /* The merge, from the strip's own signal to the store and back to a rebuilt strip: two names
     * on two files, one merge call, and the strip is the one that changes shape. The context menu
     * that produces the signal needs a hand a headless run does not have, so this drives the slot
     * the menu ends at - the wiring above that point is one `emit`. */
    const QString other = QDir(m_probe).filePath(QStringLiteral("../报告.docx"));
    const bool seededA = m_bridge->tagAdd(m_probe, {QStringLiteral("merge-b")}, QStringLiteral("manual"), 0, &ignored, &error);
    const bool seededB = m_bridge->tagAdd(other, {QStringLiteral("merge-a")}, QStringLiteral("manual"), 0, &ignored, &error);
    m_bridge->tagAdd(other, {QStringLiteral("merge-b")}, QStringLiteral("manual"), 0, &ignored, &error);
    m_tagBar->refresh(m_search->text());
    const bool chipsBefore = m_tagBar->hasChip(QStringLiteral("merge-a")) && m_tagBar->hasChip(QStringLiteral("merge-b"));
    mergeTags(QStringLiteral("merge-b"), QStringLiteral("merge-a"));
    const bool mergedOk = !m_tagBar->hasChip(QStringLiteral("merge-b"))
        && m_tagBar->hasChip(QStringLiteral("merge-a"));
    m_bridge->tagRemove(m_probe, {QStringLiteral("merge-a")}, &ignored, &error);
    m_bridge->tagRemove(other, {QStringLiteral("merge-a")}, &ignored, &error);
    m_tagBar->refresh(m_search->text());
    qInfo("self-check tag filter on %s: seeded=%d chip=%d clicked=%d settled=%d hit=%d "
          "second-click=%d folder-back=%d chip-gone=%d strip-hidden=%d merge: before=%d gone=%d",
          qUtf8Printable(QFileInfo(m_probe).fileName()), seeded ? 1 : 0, chip ? 1 : 0,
          clicked ? 1 : 0, settled ? 1 : 0, hit ? 1 : 0, second ? 1 : 0, folderBack ? 1 : 0,
          !m_tagBar->hasChip(QStringLiteral("self-check")) ? 1 : 0, m_tagBar->isHidden() ? 1 : 0,
          chipsBefore ? 1 : 0, mergedOk ? 1 : 0);
}

/* The strip as a picture: two chips, one checked and one not, because a checkable control's two
 * states are the thing a style sheet has to tell apart and increment 16's lesson is that only an
 * image says whether a new surface wore the theme. The tags come back off before it returns. */
bool MainWindow::grabTagFilterForSelfCheck(const QString &imagePath) {
    if (!m_healthy || m_probe.isEmpty() || !QFile::exists(m_probe)) {
        qWarning("self-check filter shot asked for without a file to tag");
        return false;
    }
    QJsonValue ignored;
    QString error;
    const QStringList seeded {QStringLiteral("self-check-shot"), QStringLiteral("自检查-筛选")};
    if (!m_bridge->tagAdd(m_probe, seeded, QStringLiteral("manual"), 0, &ignored, &error)) {
        qWarning("self-check filter shot could not tag the probe: %s", qUtf8Printable(error));
        return false;
    }
    m_tagBar->refresh(m_search->text());
    /* The checked chip is checked silently - `toggle` would run the search and leave the window on
     * a result set, and the frame this shot is for is about the two chip states, not the rows. */
    const bool marked = m_tagBar->markSilently(QStringLiteral("自检查-筛选"), true);
    QCoreApplication::processEvents();
    const bool saved = grab().save(imagePath);
    qInfo("self-check wrote the filter strip to %s: seeded=%d chips=%d checked=%d %s",
          qUtf8Printable(QDir::toNativeSeparators(imagePath)), seeded.size(),
          m_tagBar->chipCount(), marked ? 1 : 0, saved ? "saved" : "FAILED to save");
    m_bridge->tagRemove(m_probe, seeded, &ignored, &error);
    m_tagBar->refresh(m_search->text());
    return saved;
}

/* The row as a person sees it, with chips on it. Everything the step above asserts is about the model,
 * and a model that fills a role correctly is still compatible with a delegate that paints it in a place
 * nobody can read - which is the error increment 16 caught only by looking at a picture.
 *
 * Six tags on one file, and the window resized narrow for the frame, because a row with room for
 * everything proves nothing about the row without it: the shot is the evidence that the band counts its
 * overflow into a `+N` chip instead of eating the name, and that the kind chip yields to the tags rather
 * than the name. Only the file this run created is tagged, and the tags come back off before the
 * function returns. */
bool MainWindow::grabRowTagsForSelfCheck(const QString &imagePath) {
    if (!m_healthy || m_probe.isEmpty() || !QFile::exists(m_probe)) {
        qWarning("self-check row-tag shot asked for without a file to tag");
        return false;
    }
    FamListModel *model = active()->chain->deepestModel();
    if (model == nullptr) {
        qWarning("self-check row-tag shot: no column to paint");
        return false;
    }
    /* Bound once, at the top. `processEvents()` below can fire the timer that removes the probe file,
     * and that step clears `m_probe` when it succeeds - so a cleanup that reads the member afterwards
     * would be asking to remove tags from no path at all, and would leave its own six tags on the
     * library. This is the one reason the function cannot just use `m_probe` twice. */
    const QString probe = m_probe;
    const QStringList seeded {QStringLiteral("row-chip"), QStringLiteral("自检查"),
                              QStringLiteral("合同"), QStringLiteral("2024-归档"),
                              QStringLiteral("季度汇总-2024Q1"), QStringLiteral("需要复核")};
    QJsonValue ignored;
    QString error;
    if (!m_bridge->tagAdd(probe, seeded, QStringLiteral("manual"), 0, &ignored, &error)) {
        qWarning("self-check row-tag shot could not tag the probe: %s", qUtf8Printable(error));
        return false;
    }
    /* A note beside the chips: the dot at the row's right edge is the other fact a row can carry,
     * and this frame is the one place both are on screen together. Off again before it returns. */
    m_bridge->noteSet(probe, QStringLiteral("self-check note"), &ignored, &error);
    for (Pane *pane : {m_left, m_right}) {
        pane->invalidateTags();
    }
    pollChanges();
    for (Pane *pane : {m_left, m_right}) {
        pane->invalidateTags();
    }
    pollChanges();
    /* Counted before the column is narrowed, on the model this file's row belongs to. */
    int painted = 0;
    for (int index = 0; index < model->rowCount(); ++index) {
        if (!model->data(model->index(index), FamListModel::TagsRole).toStringList().isEmpty()) {
            ++painted;
        }
    }
    /* The probe's own row picked, before the column is narrowed. Picking a file row is what puts the
     * chips on the selected ground, and in the light scheme that ground is the very colour the chip is
     * filled with - so this is the frame that says whether the hairline alone still draws a chip there.
     * It has to be picked first: picking a file row is also what drops every column deeper than this
     * one, which is the second column this function is about to open. */
    FileView *view = active()->chain->deepestView();
    int probeRow = -1;
    for (int index = 0; index < model->rowCount(); ++index) {
        if (model->pathAt(index) == probe) {
            probeRow = index;
            break;
        }
    }
    if (probeRow >= 0 && view != nullptr && view->selectionModel() != nullptr) {
        view->selectionModel()->setCurrentIndex(model->index(probeRow),
                                                QItemSelectionModel::ClearAndSelect
                                                    | QItemSelectionModel::Rows);
    }
    /* A second column beside it, opened through the same call a click on a folder makes. One column
     * fills the pane, and a row that wide has room for anything, so it proves nothing; two of them
     * halves the first one's row, which is the width a chain is actually read at. */
    QString child;
    for (int index = 0; index < model->rowCount(); ++index) {
        if (model->data(model->index(index), FamListModel::DirectoryRole).toBool()) {
            child = model->pathAt(index);
            break;
        }
    }
    const bool narrowed = !child.isEmpty() && active()->chain->open(child);
    /* The repaint `dataChanged` queued has to have happened, or the image is the row from before the
     * chips arrived. */
    QCoreApplication::processEvents();
    const bool saved = grab().save(imagePath);
    qInfo("self-check wrote the row-tag shot to %s: seeded=%d rows-with-tags=%d narrowed=%d depth=%d %s",
          qUtf8Printable(QDir::toNativeSeparators(imagePath)), seeded.size(), painted,
          narrowed ? 1 : 0, active()->chain->depth(), saved ? "saved" : "FAILED to save");
    /* The other half of the row's overflow rule, in the same narrow column: three tags that fit only
     * once the kind chip gives up its slot. Six chips do not fit either way, so the frame above proves
     * the counter and this one proves the yield - and the untagged row beside it keeps its `document`
     * chip, which is the control that says the chip was dropped rather than never drawn. */
    const QStringList trio {QStringLiteral("合同"), QStringLiteral("自检查"),
                            QStringLiteral("2024-归档")};
    m_bridge->tagRemove(probe, seeded, &ignored, &error);
    m_bridge->tagAdd(probe, trio, QStringLiteral("manual"), 0, &ignored, &error);
    for (Pane *pane : {m_left, m_right}) {
        pane->invalidateTags();
    }
    pollChanges();
    QCoreApplication::processEvents();
    const QFileInfo second(imagePath);
    const QString yieldPath =
        second.dir().filePath(second.completeBaseName() + QStringLiteral(".yield.png"));
    const bool yielded = grab().save(yieldPath);
    qInfo("self-check wrote the kind-yield shot to %s: seeded=%d %s",
          qUtf8Printable(QDir::toNativeSeparators(yieldPath)), trio.size(),
          yielded ? "saved" : "FAILED to save");

    if (narrowed) {
        active()->chain->goBack();
    }
    m_bridge->noteClear(probe, &ignored, &error);
    m_bridge->tagRemove(probe, trio, &ignored, &error);
    /* Read back rather than trusting the call: the shot is the only step in this run that writes
     * anything the *library* keeps, and the file it wrote them on gets deleted later in the run, which
     * leaves the tags behind as rows pointing at nothing. A leaked six is a mark on the user's store
     * that no later step will clear. */
    QJsonValue left;
    m_bridge->tagList(probe, &left, &error);
    QStringList remaining;
    for (const QJsonValue &entry : left.toArray()) {
        remaining.append(entry.toObject().value(QStringLiteral("tag")).toString());
    }
    for (Pane *pane : {m_left, m_right}) {
        pane->invalidateTags();
    }
    pollChanges();
    qInfo("self-check row-tag shot cleaned up: left=[%s]", qUtf8Printable(remaining.join(u',')));
    return saved && yielded;
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

/* Waits for the queued transfers to settle - the disk assertions the transfer steps make are about
 * a transfer that finished, and with jobs the drop only queues. Pumps the loop like every other
 * wait, and bounds it: a job that has not settled in two seconds is its own headline. */
void MainWindow::waitForTransfersForSelfCheck() {
    QElapsedTimer wait;
    wait.start();
    while (wait.elapsed() < 2000 && !m_pending.isEmpty()) {
        QCoreApplication::processEvents(QEventLoop::AllEvents, 50);
    }
    if (!m_pending.isEmpty()) {
        qWarning("self-check transfer did not settle in 2s: %d job(s) still queued",
                 m_pending.size());
    }
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
    waitForTransfersForSelfCheck();
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
    waitForTransfersForSelfCheck();
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
