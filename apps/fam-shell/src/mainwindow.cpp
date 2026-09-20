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

static const char *const kKinds[] = {"directory", "image",    "video",    "audio",
                                     "document",  "archive",  "code",     "executable",
                                     "other"};

MainWindow::MainWindow(const QString &configPath, const QString &initialQuery, QWidget *parent)
    : QWidget(parent) {
    QString error;
    m_bridge = std::make_unique<FamBridge>(configPath, &error);
    if (!m_bridge->healthy()) {
        m_status = new QLabel(QStringLiteral("core unavailable: %1").arg(error), this);
        auto *layout = new QVBoxLayout(this);
        layout->addWidget(m_status);
        setLayout(layout);
        return;
    }
    if (!m_bridge->createView(QStringLiteral("name"), QStringLiteral("ascending"), &m_view, &error)) {
        m_status = new QLabel(QStringLiteral("view unavailable: %1").arg(error), this);
        auto *layout = new QVBoxLayout(this);
        layout->addWidget(m_status);
        setLayout(layout);
        return;
    }

    m_model = new FamListModel(this);

    m_search = new QLineEdit(this);
    m_search->setPlaceholderText(QStringLiteral("name terms — every word must appear"));
    m_search->setClearButtonEnabled(true);

    m_kind = new QComboBox(this);
    m_kind->addItem(QStringLiteral("any kind"), QString());
    for (const char *kind : kKinds) {
        m_kind->addItem(QString::fromLatin1(kind), QString::fromLatin1(kind));
    }

    auto *searchButton = new QPushButton(QStringLiteral("Search"), this);
    auto *watchButton = new QPushButton(QStringLiteral("Watch folder…"), this);
    auto *tagButton = new QPushButton(QStringLiteral("Tag selected"), this);

    auto *controls = new QHBoxLayout;
    controls->addWidget(m_search, 1);
    controls->addWidget(m_kind);
    controls->addWidget(searchButton);
    controls->addWidget(watchButton);
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
    connect(watchButton, &QPushButton::clicked, this, &MainWindow::watchFolder);
    connect(tagButton, &QPushButton::clicked, this, &MainWindow::tagSelectedFile);

    QJsonValue stats;
    if (m_bridge->stats(&stats, &error)) {
        const QJsonObject object = stats.toObject();
        QStringList roots;
        for (const QJsonValue &root : object.value("roots").toArray()) {
            roots.append(root.toString());
        }
        setStatus(QStringLiteral("roots: %1 · files %2 · tags %3")
                      .arg(roots.join(QStringLiteral(", ")))
                      .arg(object.value("files").toInt())
                      .arg(object.value("tags").toInt()));
    } else {
        setStatus(error);
    }

    m_timer = new QTimer(this);
    m_timer->setInterval(250);
    connect(m_timer, &QTimer::timeout, this, &MainWindow::pollDeltas);
    m_timer->start();

    if (!initialQuery.isEmpty()) {
        m_search->setText(initialQuery);
        runSearch();
    }
}

void MainWindow::runSearch() {
    QJsonValue result;
    QString error;
    const QString kind = m_kind->currentData().toString();
    if (!m_bridge->search(m_view, m_search->text(), QString(), kind, &result, &error)) {
        setStatus(error);
        return;
    }
    const QJsonObject object = result.toObject();
    m_model->load(object.value("rows").toArray());

    QString text = QStringLiteral("%1 row(s) · backend %2")
                       .arg(m_model->count())
                       .arg(object.value("backend").toString());
    if (object.value("incomplete").toBool()) {
        text += QStringLiteral(" · incomplete: scope not fully covered");
    }
    setStatus(text);
}

void MainWindow::pollDeltas() {
    if (m_bridge == nullptr || m_view == 0) {
        return;
    }
    QJsonValue result;
    QString error;
    if (!m_bridge->drain(m_view, &result, &error)) {
        if (!error.isEmpty()) {
            setStatus(error);
        }
        return;
    }

    const QJsonObject object = result.toObject();
    const QJsonArray deltas = object.value("deltas").toArray();
    if (deltas.isEmpty()) {
        return;
    }
    m_model->applyDeltas(deltas);

    const quint64 dropped = object.value("dropped").toVariant().toULongLong();
    QString text = QStringLiteral("%1 row(s) · %2 delta(s) applied")
                       .arg(m_model->count())
                       .arg(deltas.size());
    if (dropped > 0) {
        text += QStringLiteral(" · %1 event(s) dropped").arg(dropped);
    }
    if (!m_watchPath.isEmpty()) {
        text += QStringLiteral(" · watching %1").arg(m_watchPath);
    }
    setStatus(text);
}

bool MainWindow::startWatching(const QString &dir, QString *error) {
    quint64 watch = 0;
    if (!m_bridge->startWatch(dir, &watch, error)) {
        return false;
    }
    m_watch = watch;
    m_watchPath = dir;
    setStatus(QStringLiteral("watching %1").arg(dir));
    return true;
}

void MainWindow::watchFolder() {
    const QString start = m_watchPath.isEmpty() ? QDir::homePath() : m_watchPath;
    const QString dir = QFileDialog::getExistingDirectory(this, QStringLiteral("Watch folder"), start);
    if (dir.isEmpty() || m_bridge == nullptr) {
        return;
    }
    QString error;
    if (!startWatching(dir, &error)) {
        setStatus(error);
    }
}

void MainWindow::tagSelectedFile() {
    if (m_list->selectionModel() == nullptr) {
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
    runSearch();
}

void MainWindow::setStatus(const QString &text) {
    if (m_status != nullptr) {
        m_status->setText(text);
    }
}
