#include "mainwindow.h"

#include <QComboBox>
#include <QCoreApplication>
#include <QDir>
#include <QFileDialog>
#include <QHBoxLayout>
#include <QInputDialog>
#include <QJsonArray>
#include <QJsonObject>
#include <QLabel>
#include <QLineEdit>
#include <QListView>
#include <QPushButton>
#include <QScrollBar>
#include <QTimer>
#include <QUrl>
#include <QVBoxLayout>

#include "iconcache.h"

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
        setLayout(layout);
        return;
    }

    /* The kind names come from the core, in the order its records' `kind` byte means them, and with
     * them the backends it has and which can answer now. */
    m_bridge->coreBackends(&m_kinds, &m_backends, &m_available, &error);

    m_model = new FamListModel(this);
    /* The icon layer outlives every view and is shared by all of them: one worker thread, one
     * extension table, one cache. */
    m_icons = new IconCache(this);
    m_model->setIconCache(m_icons);

    m_search = new QLineEdit(this);
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
    /* Meaningless until a folder is on screen; `bindFolderView` and `showFolder` are where it lights
     * up, so the control cannot disagree with what the window is actually showing. */
    m_scope->setEnabled(false);

    auto *searchButton = new QPushButton(QStringLiteral("Search"), this);
    auto *openButton = new QPushButton(QStringLiteral("Open folder…"), this);
    auto *tagButton = new QPushButton(QStringLiteral("Tag selected"), this);

    auto *controls = new QHBoxLayout;
    controls->addWidget(m_search, 1);
    controls->addWidget(m_kind);
    controls->addWidget(m_scope);
    controls->addWidget(searchButton);
    controls->addWidget(openButton);
    controls->addWidget(tagButton);

    m_list = new QListView(this);
    m_list->setModel(m_model);
    m_list->setUniformItemSizes(true);
    m_list->setSelectionBehavior(QAbstractItemView::SelectRows);

    m_status = new QLabel(this);
    m_status->setWordWrap(true);

    auto *layout = new QVBoxLayout(this);
    layout->addLayout(controls);
    layout->addWidget(m_list, 1);
    layout->addWidget(m_status);
    setLayout(layout);

    connect(searchButton, &QPushButton::clicked, this, &MainWindow::runSearch);
    connect(m_search, &QLineEdit::returnPressed, this, &MainWindow::runSearch);
    /* Emptying the box is "stop searching", not "search for nothing": the folder that was on screen
     * before the results is still open and watched, so it comes back rather than being re-read. */
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

    if (!initialQuery.isEmpty()) {
        m_search->setText(initialQuery);
        runSearch();
    }
}

MainWindow::~MainWindow() {
    if (m_bridge == nullptr) {
        return;
    }
    if (m_dirToken != 0) {
        m_bridge->closeDir(m_dirToken);
        m_dirToken = 0;
    }
    if (m_searchToken != 0) {
        m_bridge->closeSearch(m_searchToken);
        m_searchToken = 0;
    }
}

bool MainWindow::openFolder(const QString &dir, QString *error) {
    if (m_bridge == nullptr) {
        *error = QStringLiteral("core unavailable");
        return false;
    }
    quint64 token = 0;
    if (!m_bridge->openDir(dir, &token, error)) {
        return false;
    }
    if (m_dirToken != 0) {
        m_bridge->closeDir(m_dirToken);
    }
    m_dirToken = token;
    m_directory = dir;
    if (m_searchToken != 0) {
        /* The model is about to be bound to the folder, so the search's rows are no longer on screen
         * and its token has nobody left to page them. */
        m_bridge->cancelSearch(m_searchToken);
        m_bridge->closeSearch(m_searchToken);
        m_searchToken = 0;
        m_searchRunning = false;
    }
    bindFolderView(error);
    return true;
}

void MainWindow::bindFolderView(QString *error) {
    m_scope->setEnabled(true);
    FamWindow head;
    m_bridge->dirStatus(m_dirToken, &head, error);
    const QStringList kinds = m_kinds;
    const quint64 token = m_dirToken;
    FamBridge *bridge = m_bridge.get();
    /* Painting pulls rows. The call behind this copies a bounded window out of the cache, so a
     * 78k-entry folder costs a viewport per frame and never waits on the filesystem. */
    m_model->bind(token, m_directory, kinds,
                  [bridge, token](quint32 first, quint32 count, FamWindow *window) {
                      return bridge->dirFetch(token, first, count, window, nullptr);
                  });
    /* Events that arrived between opening and binding were folded into the rows before this model
     * ever displayed them; their positions describe a view nobody had, so they are discarded here
     * rather than replayed against the wrong starting point. */
    QVector<quint32> missed;
    FamWindow drained;
    m_bridge->dirCuts(token, &drained, &missed, nullptr);
    m_bridge->dirStatus(token, &head, error);
    m_folderHead = head;
    m_model->readHead(head);
    qInfo("folder on screen: %s at %lldms: state=%u total=%u generation=%llu",
          qUtf8Printable(m_directory), m_clock.elapsed(), head.state, head.totalRows,
          head.generation);
    setStatus(folderStatus(head, QStringLiteral("following changes")));
}

void MainWindow::showFolder() {
    if (m_bridge == nullptr || m_searchToken == 0) {
        return;
    }
    m_bridge->cancelSearch(m_searchToken);
    m_bridge->closeSearch(m_searchToken);
    m_searchToken = 0;
    m_searchRunning = false;
    QString error;
    if (m_dirToken == 0) {
        /* Nothing to go back to: the results were a global search, and clearing the box leaves an
         * empty view rather than a stale result set nobody is polling any more. */
        m_scope->setEnabled(false);
        m_model->unbind();
        setStatus(QStringLiteral("type a name to search"));
        return;
    }
    bindFolderView(&error);
}

void MainWindow::openFolderDialog() {
    const QString start = m_directory.isEmpty() ? QDir::homePath() : m_directory;
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

void MainWindow::runSearch() {
    if (m_bridge == nullptr) {
        return;
    }
    QString error;
    const QString kind = m_kind->currentData().toString();
    /* The scope is the operator's, not a guess of mine: "here" is what a person means after opening
     * a folder, and "everywhere" is what the same box used to always mean. */
    const bool here = m_dirToken != 0 && m_scope->currentData().toBool();
    const QString within = here ? m_directory : QString();
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
    /* The folder stays open. It is what this search is scoped to, it is what clearing the box returns
     * to, and its watch keeps running so the rows are current on the way back - where closing it used
     * to throw the read away with the view. */
    bindResultsView(&error);
}

void MainWindow::bindResultsView(QString *error) {
    const quint64 token = m_searchToken;
    FamBridge *bridge = m_bridge.get();
    /* The same windowed fetch a directory gets, against a token whose rows are still arriving: the
     * model asks for a viewport, and the core answers with the prefix it has. */
    m_model->bind(token, QString(), m_kinds,
                  [bridge, token](quint32 first, quint32 count, FamWindow *window) {
                      return bridge->searchPage(token, first, count, window, nullptr);
                  });
    FamWindow head;
    if (!m_bridge->searchStatus(token, &head, error)) {
        setStatus(*error);
        return;
    }
    m_resultsHead = head;
    m_model->readHead(head);
    /* The line says which scope was actually asked for, because a search that quietly narrowed
     * itself is indistinguishable from one that found nothing elsewhere. */
    const bool scoped = m_dirToken != 0 && m_scope->currentData().toBool();
    qInfo("search for %s in %s queued at %lldms: state=%u total=%u",
          qUtf8Printable(m_search->text()),
          scoped ? qUtf8Printable(QDir::toNativeSeparators(m_directory))
                 : qUtf8Printable(QStringLiteral("every configured root")),
          m_clock.elapsed(), head.state, head.totalRows);
    setStatus(QStringLiteral("searching for %1").arg(m_search->text()));
}

void MainWindow::pollChanges() {
    if (m_bridge == nullptr) {
        return;
    }
    /* Both tokens are served on every beat. The folder's cuts are drained even while results are on
     * screen, because a view nobody polls queues them for as long as it lives; the model only hears
     * about the one it is showing. */
    if (m_dirToken != 0) {
        pollFolder(m_searchToken == 0);
    }
    if (m_searchToken != 0 && m_searchRunning) {
        pollSearch();
    }
}

void MainWindow::pollFolder(bool ownsModel) {
    QString error;
    /* The poll first, and its header second: the positions it hands over are relative to the rows
     * as they were *before* the patch they came from, while its header already counts that patch
     * in. Reading the status first would apply the same change twice - the row count drops to zero
     * under a list that is still displaying rows. */
    const FamWindow previous = m_folderHead;
    QVector<quint32> cuts;
    FamWindow polled;
    if (!m_bridge->dirCuts(m_dirToken, &polled, &cuts, &error)) {
        setStatus(error);
        return;
    }
    if (!ownsModel) {
        /* Results are on screen, so the model must not be told about this folder's rows - but the
         * cuts were just drained and the header adopted, which is what keeps the view current for
         * the moment the box is cleared. */
        if (polled.totalRows != previous.totalRows) {
            qInfo("folder changed behind the results: %u -> %u row(s), state=%u",
                  previous.totalRows, polled.totalRows, polled.state);
        }
        m_folderHead = polled;
        return;
    }
    if (polled.resync()) {
        /* Rows were replaced rather than patched. That used to mean "throw the view away"; now it
         * means "diff the window you have against the window the core just built", because a reset
         * costs the selection and the scroll position and a re-read of a folder where one file
         * appeared does not deserve that. The line is what the headless gate reads: `rebuilt` is the
         * fallback, and a run that only ever says `rebuilt` has not reconciled anything. */
        const auto outcome = m_model->reconcile(polled);
        m_folderHead = polled;
        qInfo("re-read reconciled in %d pair(s), %u row(s) left, %s", outcome.pairs,
              m_model->totalRows(),
              outcome.rebuilt ? "window rebuilt" : "selection kept");
        setStatus(folderStatus(polled, QStringLiteral("re-read")));
        return;
    }
    /* Timed on its own clock because the number the gate wants is "how long did the view wait for
     * these signals", which the process-elapsed figure in the line below cannot answer. */
    QElapsedTimer applied;
    applied.start();
    const int pairs = m_model->applyCuts(cuts);
    const qint64 cutMs = applied.elapsed();
    /* Always agree the count with the header, cuts or none: a poll that carried nothing can still be
     * the first moment the loader's rows exist, and the model has to hear about that from somewhere.
     * After a batch of cuts the two already match, so this is a no-op. */
    m_model->readHead(polled);
    m_folderHead = polled;
    if (polled.totalRows != previous.totalRows) {
        qInfo("rows %u -> %u with %d cut(s) at %lldms, state=%u generation=%llu",
              previous.totalRows, polled.totalRows, cuts.size(), m_clock.elapsed(), polled.state,
              polled.generation);
    }
    if (!cuts.isEmpty()) {
        /* The headless gate reads this: a cut that reached the model is the whole point of the delta
         * channel, and a screenshot alone cannot tell it apart from a repaint that refetched. */
        qInfo("cut %d position(s) in %d signal pair(s) in %lldms, %u row(s) left", cuts.size(),
              pairs, cutMs, m_model->totalRows());
    }
    setStatus(folderStatus(polled, cuts.isEmpty()
                                         ? QString()
                                         : QStringLiteral("%1 row(s) removed in %2 signal(s)")
                                               .arg(cuts.size())
                                               .arg(pairs)));
}

void MainWindow::pollSearch() {
    QString error;
    FamWindow polled;
    if (!m_bridge->searchStatus(m_searchToken, &polled, &error)) {
        m_searchRunning = false;
        setStatus(error);
        return;
    }
    m_model->readHead(polled);
    m_resultsHead = polled;
    if (polled.state == FAM_LOADING) {
        setStatus(QStringLiteral("%1 row(s) for %2 · still searching")
                      .arg(m_model->totalRows())
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
                  .arg(m_model->totalRows())
                  .arg(m_search->text())
                  .arg(why, backend));
}

void MainWindow::tagSelectedFile() {
    if (m_bridge == nullptr || m_list->selectionModel() == nullptr) {
        return;
    }
    const QString path = m_model->pathAt(m_list->selectionModel()->currentIndex().row());
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
    /* A tag changes what the row displays, and the directory view learns that on its own; a search
     * result has to be re-run to keep its rows in step. */
    if (m_dirToken == 0) {
        runSearch();
    } else {
        setStatus(QStringLiteral("tagged %1").arg(QFileInfo(path).fileName()));
    }
}

void MainWindow::setStatus(const QString &text) {
    if (m_status != nullptr) {
        m_status->setText(text);
    }
}

QString MainWindow::folderStatus(const FamWindow &head, const QString &extra) const {
    QString text = QStringLiteral("%1 · %2 row(s)")
                       .arg(QDir::toNativeSeparators(m_directory))
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
    if (m_bridge == nullptr) {
        return QString();
    }
    QStringList kinds, backends, available;
    QString ignored;
    if (!m_bridge->coreBackends(&kinds, &backends, &available, &ignored)) {
        return QString();
    }
    m_backends = backends;
    m_available = available;
    if (available.contains(QStringLiteral("everything"))) {
        return QString();
    }
    /* The one sentence the interface could never say before: that the index is not answering and
     * every name search is walking the tree instead. It changes the expected latency by orders of
     * magnitude, so it belongs on screen rather than in a JSON payload nobody reads. */
    return QStringLiteral(" · enumerating for names (no index answering)");
}

void MainWindow::scrollForSelfCheck() {
    if (m_list == nullptr || m_list->verticalScrollBar() == nullptr) {
        return;
    }
    QScrollBar *bar = m_list->verticalScrollBar();
    const int max = bar->maximum();
    /* Nine stops then home: a viewport shows about seventeen rows, so fewer stops would leave bands
     * of the folder unpainted - and an extension nobody painted is an extension the icon worker was
     * never asked about, which is exactly what the HICON counters are meant to have exercised.
     * processEvents plus repaint is what makes each stop actually paint: a deferred paint would
     * collapse all nine into one paint of wherever the bar ended. The range is logged because a
     * storm over a zero range would prove nothing and look identical in the counters. */
    for (int step = 0; step <= 8; ++step) {
        bar->setValue(max * step / 8);
        QCoreApplication::processEvents();
        m_list->viewport()->repaint();
    }
    bar->setValue(0);
    QCoreApplication::processEvents();
    m_list->viewport()->repaint();
    qInfo("self-check scrolled a %d-step range, ended at %d", max, bar->value());
}

void MainWindow::createSelfCheckProbe() {
    if (m_dirToken == 0 || m_directory.isEmpty()) {
        qWarning("self-check mutation asked for without a watched folder");
        return;
    }
    m_probe = QDir(m_directory).filePath(QStringLiteral("fam-self-check-probe.txt"));
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
