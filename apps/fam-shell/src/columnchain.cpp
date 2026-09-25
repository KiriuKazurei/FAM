#include "columnchain.h"

#include <QDir>
#include <QFileInfo>
#include <QHBoxLayout>
#include <QItemSelectionModel>
#include <QLabel>
#include <QListView>
#include <QPushButton>
#include <QVBoxLayout>

#include "famlistmodel.h"
#include "iconcache.h"
#include "rowdelegate.h"
#include "theme.h"

namespace {

/* The narrowest a column may be squeezed. Wide enough that a name plus a size is not cut off
 * mid-digit; below it the reader is looking at a scrollbar instead of a filename.
 *
 * There is deliberately no widest. The old 460px cap is what left a strip of empty window beside a
 * single open column, and the size a column can reach is bounded from the other end anyway: five
 * columns at 150px is 750px, which is what fits a default window. */
constexpr int kColumnMinWidth = 150;

/* Depth is bounded by the window, not by the filesystem: past a handful of columns the reader is
 * looking at slivers. Going deeper than this drops the oldest column, the way Explorer scrolls. */
constexpr int kMaxColumns = 5;

const char *const kSortCycle[] = {"name", "size", "modified", "kind"};

QString sortLabel(const QString &key) {
    if (key == QLatin1String("size")) {
        return QStringLiteral("Size");
    }
    if (key == QLatin1String("modified")) {
        return QStringLiteral("Date");
    }
    if (key == QLatin1String("kind")) {
        return QStringLiteral("Type");
    }
    return QStringLiteral("Name");
}

QString arrowFor(const QString &order) {
    /* Literal UTF-8, not "\xE2\x96\xB2": QStringLiteral hands those bytes to QString as three
     * Latin-1 characters, which renders as "â¶²" on screen. The em dash and middot the rest of the
     * shell already prints come through fine, so the source encoding is not the problem - the
     * escape is. */
    return order == QLatin1String("ascending") ? QStringLiteral(" ▲") : QStringLiteral(" ▼");
}

} /* namespace */

ColumnChain::ColumnChain(FamBridge *bridge, IconCache *icons, const QStringList &kinds,
                         QWidget *parent)
    : QWidget(parent), m_bridge(bridge), m_icons(icons), m_kinds(kinds) {
    m_layout = new QHBoxLayout(this);
    m_layout->setContentsMargins(0, 0, 0, 0);
    m_layout->setSpacing(2);
}

ColumnChain::Column *ColumnChain::addColumn(const QString &path, quint64 token) {
    auto *column = new Column;
    column->token = token;
    column->path = path;
    column->frame = new QWidget(this);
    column->frame->setObjectName(QString::fromLatin1(Theme::columnFrameName()));

    column->model = new FamListModel(column->frame);
    column->model->setIconCache(m_icons);
    column->view = new FileView(column->frame);
    column->view->setObjectName(QString::fromLatin1(Theme::listName()));
    column->view->setModel(column->model);
    column->view->setItemDelegate(new RowDelegate(&Theme::instance(), kFamRowRoles, column->view));
    /* This column *is* a folder, so it is a place a drag can land. A column's directory never changes
     * while it exists - a different folder gets a different column - which is why the target is named
     * once here rather than re-asked on every drop. */
    column->view->setDropFolder(path);
    QObject::connect(column->view, &FileView::dropped, this, &ColumnChain::dropped);
    /* Hover is a row state the delegate reads, and a view only tracks it when asked. */
    column->view->setMouseTracking(true);
    column->view->viewport()->setAttribute(Qt::WA_Hover, true);
    column->view->setFrameShape(QFrame::NoFrame);
    column->view->setUniformItemSizes(true);
    column->view->setSelectionBehavior(QAbstractItemView::SelectRows);
    column->view->setSelectionMode(QAbstractItemView::SingleSelection);
    column->view->setResizeMode(QListView::Adjust);
    column->view->setHorizontalScrollBarPolicy(Qt::ScrollBarAlwaysOff);

    /* A button rather than a QHeaderView: the plan wanted the sort indicator on each column's
     * QHeaderView, but a header view needs a model that answers headerData for the horizontal
     * orientation, and a list model answers for one vertical column. A flat button says the same
     * thing to the reader without a proxy model in front of it.
     *
     * The path label beside it is what makes the row of columns readable: with three of them open,
     * "Name ▲" on its own says which column is being sorted but not what is in it. */
    auto *header = new QWidget(column->frame);
    header->setObjectName(QString::fromLatin1(Theme::columnHeaderName()));
    auto *title = new QLabel(header);
    title->setObjectName(QString::fromLatin1(Theme::pathName()));
    title->setText(QDir::toNativeSeparators(column->path));
    /* A QLabel clips rather than elides, so the full path goes where hovering reaches it: a column
     * that is 230px wide cannot show `D:\Datas\...\深入一层` in any spelling. */
    title->setToolTip(column->path);
    /* And it has to be told it may be clipped at all. A label's own minimum width is its full text,
     * so in a header strip narrower than the path the layout over-commits and the label is drawn on
     * top of the sort button - which is what three columns in a split window look like. Yielding
     * everything it cannot show keeps the button's room, and the tooltip keeps the path's. */
    title->setMinimumWidth(0);
    title->setSizePolicy(QSizePolicy::Ignored, QSizePolicy::Preferred);
    auto *button = new QPushButton(sortLabel(column->sort) + arrowFor(column->order), header);
    button->setObjectName(QString::fromLatin1(Theme::sortHeaderName()));
    button->setFlat(true);
    button->setCursor(Qt::PointingHandCursor);
    auto *strip = new QHBoxLayout(header);
    strip->setContentsMargins(Theme::instance().tokens().sm, 0, Theme::instance().tokens().xs, 0);
    strip->setSpacing(0);
    strip->addWidget(title, 1);
    strip->addWidget(button, 0, Qt::AlignVCenter);
    QObject::connect(button, &QPushButton::clicked, this, [this, token] {
        if (Column *column = find(token)) {
            onHeaderClicked(*column);
        }
    });

    auto *box = new QVBoxLayout(column->frame);
    box->setContentsMargins(0, 0, 0, 0);
    box->setSpacing(0);
    box->addWidget(header);
    box->addWidget(column->view, 1);
    column->frame->setMinimumWidth(kColumnMinWidth);
    /* No maximum. A pane with one column open should show one column that fills the pane - capping
     * it left a strip of empty window to the right, which reads as a broken layout rather than as a
     * narrow column. How wide the columns get is bounded from the other side instead: the depth cap
     * limits how many can be open at once, and `kColumnMinWidth` keeps each of them readable, so the
     * worst case is `kMaxDepth` columns at 150px and nothing below that. */
    column->frame->setSizePolicy(QSizePolicy::Expanding, QSizePolicy::Expanding);

    const quint64 held = token;
    FamBridge *bridge = m_bridge;
    column->model->bind(token, path, m_kinds,
                        [bridge, held](quint32 first, quint32 count, FamWindow *window) {
                            return bridge->dirFetch(held, first, count, window, nullptr);
                        });

    QObject::connect(column->view->selectionModel(), &QItemSelectionModel::currentChanged, this,
                     [this, token](const QModelIndex &, const QModelIndex &previous) {
                         onCurrentChanged(token, previous);
                     });

    m_layout->addWidget(column->frame);
    m_columns.append(column);
    return column;
}

void ColumnChain::dropColumn(Column *column) {
    m_bridge->closeDir(column->token);
    /* The frame owns the view, the header button and the model, so deleting it releases the whole
     * column - which is also what tells the core to stop watching the directory, since the last view
     * of a directory is what keeps its watch armed. */
    m_layout->removeWidget(column->frame);
    column->frame->deleteLater();
    delete column;
}

void ColumnChain::dropDeepest() {
    if (m_columns.isEmpty()) {
        return;
    }
    dropColumn(m_columns.takeLast());
}

bool ColumnChain::open(const QString &path) {
    /* Miller semantics: a folder already on screen truncates the chain to it (its children are what
     * comes next), and a folder that is not on screen yet is *appended*. Getting this wrong is
     * invisible until someone descends - replacing the chain instead of appending leaves one column
     * showing the child and no column showing where it came from. */
    for (int existing = 0; existing < m_columns.size(); ++existing) {
        if (m_columns.at(existing)->path == path) {
            while (m_columns.size() > existing + 1) {
                dropDeepest();
            }
            return true;
        }
    }
    quint64 token = 0;
    QString error;
    if (!m_bridge->openDir(path, &token, &error)) {
        return false;
    }
    Column *column = addColumn(path, token);
    while (m_columns.size() > kMaxColumns) {
        dropColumn(m_columns.takeFirst());
    }

    /* Events that arrived between opening and binding were folded into the rows before this model
     * ever displayed them; their positions describe a view nobody had, so they are discarded here
     * rather than replayed against the wrong starting point. */
    QVector<quint32> missed;
    FamWindow drained;
    m_bridge->dirCuts(column->token, &drained, &missed, nullptr);
    FamWindow head;
    m_bridge->dirStatus(column->token, &head, nullptr);
    column->head = head;
    column->model->readHead(head);
    return true;
}

bool ColumnChain::openRoot(const QString &path) {
    /* "Go to this folder" from the location side, not from the chain: everything the reader was
     * looking at is replaced, so the new folder starts the chain rather than joining it. */
    clear();
    return open(path);
}

QString ColumnChain::goBack() {
    if (m_columns.isEmpty()) {
        return QString();
    }
    dropDeepest();
    return deepestPath();
}

void ColumnChain::clear() {
    while (!m_columns.isEmpty()) {
        dropDeepest();
    }
}

FamWindow ColumnChain::deepestHead() const {
    return m_columns.isEmpty() ? FamWindow() : m_columns.last()->head;
}

QString ColumnChain::selectedPath() const {    if (m_columns.isEmpty()) {
        return QString();
    }
    const Column &column = *m_columns.last();
    const QModelIndex current = column.view->selectionModel()->currentIndex();
    if (current.isValid()) {
        const QString path = column.model->pathAt(current.row());
        if (!path.isEmpty()) {
            return path;
        }
    }
    return column.path;
}

bool ColumnChain::selectFirstFolder() {
    if (m_columns.isEmpty()) {
        return false;
    }
    Column &column = *m_columns.last();
    const int rows = column.model->rowCount();
    for (int row = 0; row < rows; ++row) {
        const QModelIndex index = column.model->index(row);
        if (!column.model->data(index, FamListModel::DirectoryRole).toBool()) {
            continue;
        }
        m_filling = false;
        column.view->selectionModel()->setCurrentIndex(
            index, QItemSelectionModel::ClearAndSelect | QItemSelectionModel::Rows);
        /* currentChanged fired, and the handler opened the folder - which is the whole point: this
         * returns true only if a column actually appeared, so a caller can assert on the depth. */
        return m_columns.size() > 1;
    }
    return false;
}

QString ColumnChain::poll() {
    QString changed;
    for (Column *column : m_columns) {
        QVector<quint32> cuts;
        FamWindow polled;
        QString error;
        if (!m_bridge->dirCuts(column->token, &polled, &cuts, &error)) {
            changed += error + QLatin1Char(' ');
            continue;
        }
        const quint32 before = column->head.totalRows;
        if (polled.resync()) {
            const auto outcome = column->model->reconcile(polled);
            /* Both numbers, because they are two different costs: a pair is a row range the view has
             * to re-lay-out around, and a modified row is one it only has to repaint. A re-read that
             * spends pairs on rows that never moved is the shape that used to steal the reader's
             * current row out from under them. */
            qInfo("column %s reconciled in %d pair(s) + %d modified row(s), %u row(s), %s",
                  qUtf8Printable(QDir::toNativeSeparators(column->path)), outcome.pairs,
                  outcome.modified, column->model->totalRows(),
                  outcome.rebuilt ? "window rebuilt" : "selection kept");
        } else {
            const int pairs = column->model->applyCuts(cuts);
            column->model->readHead(polled);
            if (!cuts.isEmpty()) {
                qInfo("column %s cut %d position(s) in %d pair(s), %u row(s) left",
                      qUtf8Printable(QDir::toNativeSeparators(column->path)), cuts.size(), pairs,
                      column->model->totalRows());
            }
        }
        column->head = polled;
        if (polled.totalRows != before) {
            /* Logged as well as returned: "the rows arrived" was a gate line when one column existed,
             * and a sentence in the status bar is not something the headless run can assert on. */
            qInfo("column %s rows %u -> %u, state=%u generation=%llu",
                  qUtf8Printable(QDir::toNativeSeparators(column->path)), before,
                  polled.totalRows, polled.state, polled.generation);
            changed += QStringLiteral("%1 %2→%3 row(s)  ")
                           .arg(QFileInfo(column->path).fileName().isEmpty()
                                    ? column->path
                                    : QFileInfo(column->path).fileName())
                           .arg(before)
                           .arg(polled.totalRows);
        }
    }
    return changed.trimmed();
}

QString ColumnChain::sortDeepest() {
    if (m_columns.isEmpty()) {
        return QString();
    }
    Column &column = *m_columns.last();
    onHeaderClicked(column);
    return column.sort + QLatin1Char(' ') + column.order;
}

bool ColumnChain::clickDeepestHeader() {
    if (m_columns.isEmpty()) {
        return false;
    }
    QPushButton *button =
        m_columns.last()->frame->findChild<QPushButton *>(QStringLiteral("sortHeader"));
    if (button == nullptr) {
        return false;
    }
    button->click();
    return true;
}

void ColumnChain::onHeaderClicked(Column &column) {
    /* The keys cycle in the order a reader expects from a file list, and the second click on the same
     * key flips the direction - so one control reaches every (key, order) pair the core accepts, in
     * four clicks per key. */
    int index = 0;
    for (int probe = 0; probe < 4; ++probe) {
        if (column.sort == QLatin1String(kSortCycle[probe])) {
            index = probe;
            break;
        }
    }
    if (column.order == QLatin1String("ascending")) {
        column.order = QStringLiteral("descending");
    } else {
        column.order = QStringLiteral("ascending");
        index = (index + 1) % 4;
        column.sort = QLatin1String(kSortCycle[index]);
    }
    applySortTo(column);
}

void ColumnChain::applySortTo(Column &column) {
    QString error;
    if (!m_bridge->dirSetSort(column.token, column.sort, column.order, &error)) {
        qWarning("cannot sort %s: %s", qUtf8Printable(column.path), qUtf8Printable(error));
        return;
    }
    if (auto *button = column.frame->findChild<QPushButton *>(QStringLiteral("sortHeader"))) {
        button->setText(sortLabel(column.sort) + arrowFor(column.order));
    }
    /* The core marks the view for a resync rather than reordering it in place, so the rows change on
     * the next beat. Polling now keeps the header's arrow and the rows in step within the same click
     * instead of showing one for a quarter second. */
    poll();
}

ColumnChain::Column *ColumnChain::find(quint64 token) {
    for (Column *column : m_columns) {
        if (column->token == token) {
            return column;
        }
    }
    return nullptr;
}

int ColumnChain::indexOf(const Column *wanted) const {
    for (int position = 0; position < m_columns.size(); ++position) {
        if (m_columns.at(position) == wanted) {
            return position;
        }
    }
    return -1;
}

void ColumnChain::onCurrentChanged(quint64 token, const QModelIndex &previous) {
    Column *found = find(token);
    if (found == nullptr) {
        /* The column was dropped while a queued selection change was still in flight. */
        return;
    }
    Column &column = *found;
    const QModelIndex current = column.view->selectionModel()->currentIndex();
    const bool valid = current.isValid();
    const QString path = valid ? column.model->pathAt(current.row()) : QString();
    const bool isDirectory = valid && column.model->data(current, FamListModel::DirectoryRole).toBool();
    /* Every change of chain shape from here on is this row's doing, so the row is what belongs in the
     * log - and the row it came from, because "the reader moved down one" and "a model reset
     * re-resolved the current index onto whatever sits at that number now" look identical afterwards
     * and differ entirely in what they mean. Printed even when the change is suppressed: a suppressed
     * change still happened, and a chain that grew on its own has to be traceable to the signal. */
    qInfo("column %s: current row %d (was %d, %d listed)%s, %s is %s",
          qUtf8Printable(QDir::toNativeSeparators(column.path)), valid ? current.row() : -1,
          previous.row(), column.model->rowCount(), m_filling ? " suppressed" : "",
          qUtf8Printable(QFileInfo(path).fileName()),
          !valid ? "nothing" : (isDirectory ? "a folder" : "a file"));
    if (m_filling || !valid) {
        return;
    }
    if (isDirectory) {
        m_filling = true;
        open(path);
        m_filling = false;
        return;
    }
    /* A file was picked: anything deeper than this column is showing a folder nobody asked for. */
    m_filling = true;
    const int position = indexOf(&column);
    while (position >= 0 && m_columns.size() > position + 1) {
        dropDeepest();
    }
    m_filling = false;
}
