#include "mainwindow.h"

#include <QComboBox>
#include <QCoreApplication>
#include <QDir>
#include <QFileDialog>
#include <QFileInfo>
#include <QHBoxLayout>
#include <QInputDialog>
#include <QJsonArray>
#include <QJsonObject>
#include <QKeyEvent>
#include <QLabel>
#include <QLineEdit>
#include <QListView>
#include <QPushButton>
#include <QScrollBar>
#include <QStackedWidget>
#include <QTimer>
#include <QUrl>
#include <QVBoxLayout>

#include "drivelist.h"
#include "iconcache.h"
#include "rowdelegate.h"
#include "theme.h"

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
    auto *tagButton = new QPushButton(QStringLiteral("Tag selected"), this);

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

    m_chain = new ColumnChain(m_bridge.get(), m_icons, m_kinds, this);

    m_resultsModel = new FamListModel(this);
    m_resultsModel->setIconCache(m_icons);
    m_results = new QListView(this);
    m_results->setObjectName(QString::fromLatin1(Theme::listName()));
    m_results->setModel(m_resultsModel);
    m_results->setUniformItemSizes(true);
    m_results->setSelectionBehavior(QAbstractItemView::SelectRows);
    m_results->setResizeMode(QListView::Adjust);
    /* The delegate paints its own ground, so hover only arrives if the view is told to track it -
     * and a row that cannot show its hover state is a row whose selected state is the only feedback a
     * person gets about where the cursor is. */
    m_results->setItemDelegate(new RowDelegate(&Theme::instance(), kFamRowRoles, m_results));
    m_results->setMouseTracking(true);
    m_results->viewport()->setAttribute(Qt::WA_Hover, true);

    /* Two pages, not one model wearing two hats: the columns stay live behind the results, so coming
     * back from a search is a page flip rather than a re-read of every folder in the chain. */
    m_pages = new QStackedWidget(this);
    m_pages->addWidget(m_chain);
    m_pages->addWidget(m_results);

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
    body->addWidget(m_drives);
    body->addWidget(m_pages, 1);

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
        /* The chain was never built; there is nothing to close and `m_chain` is null. */
        return;
    }
    /* The chain closes the directories it holds, and that has to happen here rather than in its own
     * destructor: a QObject child is destroyed after this class's members, and `m_bridge` owns the
     * handle those calls need. Same reason for the search token. */
    m_chain->clear();
    if (m_searchToken != 0) {
        m_bridge->closeSearch(m_searchToken);
        m_searchToken = 0;
    }
}

bool MainWindow::openFolder(const QString &dir, QString *error) {
    if (!m_healthy) {
        *error = QStringLiteral("core unavailable");
        return false;
    }
    if (!m_chain->openRoot(dir)) {
        *error = QStringLiteral("cannot open %1").arg(QDir::toNativeSeparators(dir));
        return false;
    }
    if (m_searchToken != 0) {
        /* A folder on screen replaces the results, and the search that produced them is cancelled
         * rather than left running to fill a token nobody pages any more. */
        m_bridge->cancelSearch(m_searchToken);
        m_bridge->closeSearch(m_searchToken);
        m_searchToken = 0;
        m_searchRunning = false;
    }
    m_pages->setCurrentWidget(m_chain);
    refreshScopeAvailability();
    const FamWindow head = m_chain->deepestHead();
    qInfo("folder on screen: %s at %lldms: state=%u total=%u generation=%llu",
          qUtf8Printable(QDir::toNativeSeparators(dir)), m_clock.elapsed(), head.state,
          head.totalRows, head.generation);
    setStatus(folderStatus(dir, head, QStringLiteral("following changes")));
    return true;
}

void MainWindow::openFolderDialog() {
    const QString start = m_chain->isEmpty() ? QDir::homePath() : m_chain->deepestPath();
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
    if (!m_healthy || m_searchToken == 0) {
        return;
    }
    m_bridge->cancelSearch(m_searchToken);
    m_bridge->closeSearch(m_searchToken);
    m_searchToken = 0;
    m_searchRunning = false;
    if (m_chain->isEmpty()) {
        /* Nothing to go back to: the results were a search over every root, and clearing the box
         * leaves an empty view rather than a result set nobody is paging. */
        m_resultsModel->unbind();
        setStatus(QStringLiteral("type a name to search"));
        return;
    }
    m_pages->setCurrentWidget(m_chain);
    refreshScopeAvailability();
    setStatus(folderStatus(m_chain->deepestPath(), m_chain->deepestHead(),
                           QStringLiteral("following changes")));
}

void MainWindow::showResults() {
    m_pages->setCurrentWidget(m_results);
    refreshScopeAvailability();
}

void MainWindow::refreshScopeAvailability() {
    m_scope->setEnabled(!m_chain->isEmpty());
}

void MainWindow::runSearch() {
    if (!m_healthy) {
        return;
    }
    QString error;
    const QString kind = m_kind->currentData().toString();
    /* The scope is the operator's, not a guess: "here" is what a person means after opening a folder,
     * and "everywhere" is what this box used to always mean. Either one alone is a regression. */
    const bool here = !m_chain->isEmpty() && m_scope->currentData().toBool();
    const QString within = here ? m_chain->deepestPath() : QString();
    quint64 token = 0;
    if (!m_bridge->beginSearch(m_search->text(), within, kind, 0, &token, &error)) {
        setStatus(error);
        return;
    }
    if (m_searchToken != 0) {
        m_bridge->closeSearch(m_searchToken);
    }
    m_searchToken = token;
    m_searchRunning = true;

    const quint64 held = token;
    FamBridge *bridge = m_bridge.get();
    /* The same windowed fetch a column gets, against a token whose rows are still arriving: the model
     * asks for a viewport, and the core answers with the prefix it has. */
    m_resultsModel->bind(token, QString(), m_kinds,
                         [bridge, held](quint32 first, quint32 count, FamWindow *window) {
                             return bridge->searchPage(held, first, count, window, nullptr);
                         });
    FamWindow head;
    if (!m_bridge->searchStatus(token, &head, &error)) {
        setStatus(error);
        return;
    }
    m_resultsModel->readHead(head);
    showResults();
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
    /* Both halves are served on every beat. The columns are polled even while results are on screen,
     * because a view nobody drains queues its cut positions for as long as it lives. */
    const QString changed = m_chain->poll();
    if (!changed.isEmpty() && m_pages->currentWidget() == m_chain) {
        setStatus(folderStatus(m_chain->deepestPath(), m_chain->deepestHead(), changed));
    }
    if (m_searchToken != 0 && m_searchRunning) {
        pollSearch();
    }
}

void MainWindow::pollSearch() {
    QString error;
    FamWindow polled;
    if (!m_bridge->searchStatus(m_searchToken, &polled, &error)) {
        m_searchRunning = false;
        setStatus(error);
        return;
    }
    m_resultsModel->readHead(polled);
    if (polled.state == FAM_LOADING) {
        setStatus(QStringLiteral("%1 row(s) for %2 · still searching")
                      .arg(m_resultsModel->totalRows())
                      .arg(m_search->text()));
        return;
    }
    m_searchRunning = false;
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
    setStatus(QStringLiteral("%1 row(s) for %2%3%4")
                  .arg(m_resultsModel->totalRows())
                  .arg(m_search->text())
                  .arg(why, backend));
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
    const bool showingResults = m_pages->currentWidget() == m_results;
    QString path;
    if (showingResults) {
        if (m_results->selectionModel() == nullptr) {
            return;
        }
        path = m_resultsModel->pathAt(m_results->selectionModel()->currentIndex().row());
    } else {
        path = m_chain->selectedPath();
    }
    if (path.isEmpty()) {
        setStatus(QStringLiteral("select a row first"));
        return;
    }

    bool accepted = false;
    const QString tag = QInputDialog::getText(this, QStringLiteral("Add tag"),
                                              QStringLiteral("tag for %1").arg(path),
                                              QLineEdit::Normal, QString(), &accepted);
    if (!accepted || tag.trimmed().isEmpty()) {
        return;
    }

    QJsonValue result;
    QString error;
    if (!m_bridge->tagAdd(path, QStringList{tag.trimmed()}, QStringLiteral("manual"), 0, &result,
                          &error)) {
        setStatus(error);
        return;
    }
    /* A tag changes what the row displays, and a folder view learns that on its own; a search result
     * has to be re-run to keep its rows in step. */
    if (showingResults) {
        runSearch();
    } else {
        setStatus(QStringLiteral("tagged %1").arg(QFileInfo(path).fileName()));
    }
}

void MainWindow::keyPressEvent(QKeyEvent *event) {
    const bool back = event->key() == Qt::Key_Backspace
                      || (event->key() == Qt::Key_Left
                          && event->modifiers().testFlag(Qt::AltModifier));
    if (back && m_chain != nullptr && !m_chain->isEmpty()) {
        const QString path = m_chain->goBack();
        refreshScopeAvailability();
        if (path.isEmpty()) {
            setStatus(QStringLiteral("no folder open"));
        } else {
            setStatus(folderStatus(path, m_chain->deepestHead(), QStringLiteral("went up one")));
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
    QListView *view = m_chain == nullptr ? nullptr : m_chain->deepestView();
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
    const QString folder = m_chain->deepestPath();
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
    if (m_probe.isEmpty()) {
        return;
    }
    const bool gone = QFile::remove(m_probe);
    qInfo("self-check removed the probe: %s", gone ? "yes" : "no");
    m_probe.clear();
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
    const int before = m_chain->depth();
    m_chain->selectFirstFolder();
    qInfo("self-check descended: depth %d -> %d", before, m_chain->depth());
}

void MainWindow::sortDeepestForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    const QString applied = m_chain->sortDeepest();
    qInfo("self-check sorted the deepest column by %s", qUtf8Printable(applied));
}

void MainWindow::clickHeaderForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    const bool clicked = m_chain->clickDeepestHeader();
    qInfo("self-check clicked the deepest column's header: %s, chain depth %d",
          clicked ? "yes" : "no", m_chain->depth());
}

void MainWindow::pickFirstDriveForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    const QString root = m_drives->selectFirst();
    qInfo("self-check picked drive %s, chain depth %d", qUtf8Printable(root), m_chain->depth());
}

void MainWindow::goUpForSelfCheck() {
    if (!m_healthy) {
        return;
    }
    const QString remained = m_chain->goBack();
    refreshScopeAvailability();
    qInfo("self-check went up to %s, chain depth %d",
          qUtf8Printable(QDir::toNativeSeparators(remained)), m_chain->depth());
}
