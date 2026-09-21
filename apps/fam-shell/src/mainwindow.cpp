#include "mainwindow.h"

#include <QComboBox>
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
#include <QTimer>
#include <QUrl>
#include <QVBoxLayout>

MainWindow::MainWindow(const QString &configPath, const QString &initialQuery, QWidget *parent)
    : QWidget(parent) {
    m_clock.start();
    QString error;
    m_bridge = std::make_unique<FamBridge>(configPath, &error);
    if (!m_bridge->healthy()) {
        m_status = new QLabel(QStringLiteral("core unavailable: %1").arg(error), this);
        auto *layout = new QVBoxLayout(this);
        layout->addWidget(m_status);
        setLayout(layout);
        return;
    }

    /* The kind names come from the core, in the order its records' `kind` byte means them. */
    m_bridge->coreKinds(&m_kinds, &error);

    m_model = new FamListModel(this);

    m_search = new QLineEdit(this);
    m_search->setPlaceholderText(QStringLiteral("name terms — every word must appear"));
    m_search->setClearButtonEnabled(true);

    m_kind = new QComboBox(this);
    m_kind->addItem(QStringLiteral("any kind"), QString());
    for (const QString &kind : m_kinds) {
        m_kind->addItem(kind, kind);
    }

    auto *searchButton = new QPushButton(QStringLiteral("Search"), this);
    auto *openButton = new QPushButton(QStringLiteral("Open folder…"), this);
    auto *tagButton = new QPushButton(QStringLiteral("Tag selected"), this);

    auto *controls = new QHBoxLayout;
    controls->addWidget(m_search, 1);
    controls->addWidget(m_kind);
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
    if (m_bridge != nullptr && m_dirToken != 0) {
        m_bridge->closeDir(m_dirToken);
        m_dirToken = 0;
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

    FamWindow head;
    m_bridge->dirStatus(token, &head, error);
    const QStringList kinds = m_kinds;
    FamBridge *bridge = m_bridge.get();
    /* Painting pulls rows. The call behind this copies a bounded window out of the cache, so a
     * 78k-entry folder costs a viewport per frame and never waits on the filesystem. */
    m_model->bind(token, dir, kinds, [bridge, token](quint32 first, quint32 count,
                                                     FamWindow *window) {
        return bridge->dirFetch(token, first, count, window, nullptr);
    });
    /* Events that arrived between opening and binding were folded into the rows before this model
     * ever displayed them; their positions describe a view nobody had, so they are discarded here
     * rather than replayed against the wrong starting point. */
    QVector<quint32> missed;
    FamWindow drained;
    m_bridge->dirCuts(token, &drained, &missed, nullptr);
    m_bridge->dirStatus(token, &head, error);
    m_head = head;
    m_model->readHead(head);
    qInfo("opened %s at %lldms: state=%u total=%u generation=%llu", qUtf8Printable(dir),
          m_clock.elapsed(), head.state, head.totalRows, head.generation);
    setStatus(QStringLiteral("%1 · %2 row(s) · following changes")
                  .arg(QDir::toNativeSeparators(dir))
                  .arg(head.totalRows));
    return true;
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

bool MainWindow::ensureSearchView(QString *error) {
    if (m_view != 0) {
        return true;
    }
    return m_bridge->createView(QStringLiteral("name"), QStringLiteral("ascending"), &m_view,
                                error);
}

void MainWindow::runSearch() {
    if (m_bridge == nullptr) {
        return;
    }
    QString error;
    if (!ensureSearchView(&error)) {
        setStatus(error);
        return;
    }
    const QString kind = m_kind->currentData().toString();
    QJsonValue result;
    if (!m_bridge->search(m_view, m_search->text(), QString(), kind, &result, &error)) {
        setStatus(error);
        return;
    }
    const QJsonObject object = result.toObject();

    /* Search still answers in v1's JSON, where each row carries its own full path. */
    QVector<FamViewRow> rows;
    const QJsonArray listed = object.value("rows").toArray();
    rows.reserve(listed.size());
    for (const QJsonValue &value : listed) {
        const QJsonObject row = value.toObject();
        FamViewRow entry;
        entry.name = row.value(QStringLiteral("name")).toString();
        entry.path = row.value(QStringLiteral("path")).toString();
        entry.size = row.value(QStringLiteral("size")).toVariant().toULongLong();
        entry.modifiedMs = qint64(row.value(QStringLiteral("modified_ms")).toDouble());
        entry.kind = quint8(qMax(0, m_kinds.indexOf(row.value(QStringLiteral("kind")).toString())));
        entry.directory = row.value(QStringLiteral("kind")).toString()
                          == QLatin1String("directory");
        rows.append(entry);
    }
    /* A result set has no directory to be current about, so the folder view goes away with it. */
    if (m_dirToken != 0) {
        m_bridge->closeDir(m_dirToken);
        m_dirToken = 0;
    }
    m_model->loadStatic(rows);

    QString text = QStringLiteral("%1 row(s) · backend %2")
                       .arg(m_model->count())
                       .arg(object.value(QStringLiteral("backend")).toString());
    if (object.value(QStringLiteral("incomplete")).toBool()) {
        text += QStringLiteral(" · incomplete: scope not fully covered");
    }
    setStatus(text);
}

void MainWindow::pollChanges() {
    if (m_bridge == nullptr || m_dirToken == 0) {
        return;
    }
    QString error;
    /* The poll first, and its header second: the positions it hands over are relative to the rows
     * as they were *before* the patch they came from, while its header already counts that patch
     * in. Reading the status first would apply the same change twice - the row count drops to zero
     * under a list that is still displaying rows. */
    const FamWindow previous = m_head;
    QVector<quint32> cuts;
    FamWindow polled;
    if (!m_bridge->dirCuts(m_dirToken, &polled, &cuts, &error)) {
        setStatus(error);
        return;
    }
    if (polled.resync()) {
        /* Rows were replaced rather than patched, so positions mean nothing: repaint from the top. */
        m_model->readHead(polled);
        m_head = polled;
        setStatus(QStringLiteral("%1 · %2 row(s) · rebuilt after a re-read")
                      .arg(QDir::toNativeSeparators(m_directory))
                      .arg(polled.totalRows));
        return;
    }
    const int pairs = m_model->applyCuts(cuts);
    /* Always agree the count with the header, cuts or none: a poll that carried nothing can still be
     * the first moment the loader's rows exist, and the model has to hear about that from somewhere.
     * After a batch of cuts the two already match, so this is a no-op. */
    m_model->readHead(polled);
    m_head = polled;
    if (polled.totalRows != previous.totalRows) {
        qInfo("rows %u -> %u with %d cut(s) at %lldms, state=%u generation=%llu",
              previous.totalRows, polled.totalRows, cuts.size(), m_clock.elapsed(), polled.state,
              polled.generation);
    }
    if (!cuts.isEmpty()) {
        /* The headless gate reads this: a cut that reached the model is the whole point of the delta
         * channel, and a screenshot alone cannot tell it apart from a repaint that refetched. */
        qInfo("cut %d position(s) in %d signal pair(s), %u row(s) left", cuts.size(), pairs,
              m_model->totalRows());
    }
    QString text = QStringLiteral("%1 · %2 row(s)")
                       .arg(QDir::toNativeSeparators(m_directory))
                       .arg(m_model->totalRows());
    if (!cuts.isEmpty()) {
        text += QStringLiteral(" · %1 row(s) removed in %2 signal(s)").arg(cuts.size()).arg(pairs);
    }
    if (polled.truncated()) {
        text += QStringLiteral(" · more pending");
    }
    if (polled.lostEvents()) {
        text += QStringLiteral(" · events dropped");
    }
    if (polled.state == FAM_STALE) {
        text += QStringLiteral(" · stale");
    }
    setStatus(text);
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
