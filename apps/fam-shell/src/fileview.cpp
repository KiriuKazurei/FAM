#include "fileview.h"

#include "famlistmodel.h"
#include "rowdelegate.h"

#include <QDragEnterEvent>
#include <QContextMenuEvent>
#include <QMenu>
#include <QDragMoveEvent>
#include <QDropEvent>
#include <QMimeData>

const char *const FileView::kMime = "application/x-fam-paths";

FileView::FileView(QWidget *parent) : QListView(parent) {
    /* Drag-only until a folder is named. The mode is what decides whether this view takes drops at
     * all, and a list that accepts a drop it has no destination for is worse than one that does not. */
    setDragDropMode(QAbstractItemView::DragOnly);
    /* A plain drag means move, so the drag's own default action has to say move too: Qt picks copy
     * for a list in DragDrop mode, and the cursor drawn during the drag is the only feedback about
     * what is about to happen. `dropEvent` reads the modifier rather than this, so a wrong default
     * here could not corrupt a transfer - it would only promise one kind of hand and deliver another. */
    setDefaultDropAction(Qt::MoveAction);
    /* No drop indicator. The line a list draws between two rows says "the item lands here", and a
     * file dropped into a folder has no landing position - the folder's own order decides that. */
    setDropIndicatorShown(false);
}

void FileView::setDropHover(bool on) {
    if (property("dropHover").toBool() == on) {
        return;
    }
    setProperty("dropHover", on);
    /* The sheet matches on properties, but only re-resolves a widget when told: unpolish/polish is
     * what turns the change into a repaint. */
    style()->unpolish(this);
    style()->polish(this);
    update();
}

void FileView::relayTagClick(const QModelIndex &index, const QString &tag) {
    const QString path = index.data(FamListModel::PathRole).toString();
    if (!path.isEmpty()) {
        emit tagActivated(path, tag);
    }
}

void FileView::setDropFolder(const QString &folder) {
    m_folder = folder;
    setDragDropMode(folder.isEmpty() ? QAbstractItemView::DragOnly
                                     : QAbstractItemView::DragDrop);
}

QMimeData *FileView::mimeDataForPaths(const QStringList &paths) {
    auto *data = new QMimeData;
    QByteArray payload;
    for (const QString &path : paths) {
        payload += path.toUtf8() + '\n';
    }
    /* No escaping, and no newline can need it: Windows refuses the character in a filename, so a path
     * this shell composed can never contain the separator. */
    data->setData(QString::fromLatin1(kMime), payload);
    return data;
}

QStringList FileView::pathsOf(const QMimeData *data) {
    if (data == nullptr || !data->hasFormat(QString::fromLatin1(kMime))) {
        return {};
    }
    return QString::fromUtf8(data->data(QString::fromLatin1(kMime)))
        .split(QLatin1Char('\n'), Qt::SkipEmptyParts);
}

bool FileView::takes(const QMimeData *data) const {
    /* The whole of this view's drop policy, in two clauses: it has to know a folder to drop into, and
     * the payload has to be one this shell wrote. */
    return !m_folder.isEmpty() && !pathsOf(data).isEmpty();
}

void FileView::dragEnterEvent(QDragEnterEvent *event) {
    setDropHover(takes(event->mimeData()));
    if (takes(event->mimeData())) {
        event->acceptProposedAction();
    } else {
        event->ignore();
    }
}

void FileView::dragLeaveEvent(QDragLeaveEvent *event) {
    /* Cleared even when the view was never a target, for the same reason it is set conditionally:
     * the property is the answer to "would this column take it", and a stale yes outlives the drag
     * that asked. */
    setDropHover(false);
    event->accept();
}

void FileView::dragMoveEvent(QDragMoveEvent *event) {
    /* Answered again on every move rather than once at the border: a drag manager that gets no answer
     * mid-drag treats the cursor as being over a target that changed its mind, and shows the no-drop
     * cursor. `QDragMoveEvent` is a sibling of `QDragEnterEvent` rather than a derived class, so the
     * two answers are written out twice. */
    setDropHover(takes(event->mimeData()));
    if (takes(event->mimeData())) {
        event->acceptProposedAction();
    } else {
        event->ignore();
    }
}

/* The selection changed: the preview should follow. Emitted here rather than from the selection
 * model's signal because the view knows both the path and the directory flag — the two things the
 * preview panel needs. */
void FileView::currentChanged(const QModelIndex &current, const QModelIndex &previous) {
    QListView::currentChanged(current, previous);
    if (!current.isValid()) {
        return;
    }
    const QString path = current.data(FamListModel::PathRole).toString();
    const bool isDirectory = current.data(FamListModel::DirectoryRole).toBool();
    if (!path.isEmpty()) {
        emit previewChanged(path, isDirectory);
    }
}

void FileView::keyPressEvent(QKeyEvent *event) {
    const QModelIndex current = currentIndex();
    const QString path = current.isValid()
        ? current.data(FamListModel::PathRole).toString()
        : QString();
    if (event->key() == Qt::Key_Delete && !path.isEmpty()) {
        emit deleteRequested(path);
        return;
    }
    if (event->key() == Qt::Key_F2 && !path.isEmpty()) {
        emit renameRequested(path);
        return;
    }
    /* Enter and double-click say the same thing: open this. A folder's opening already happened -
     * its selection change is what drove the chain one click ago - so only a file is announced. */
    if ((event->key() == Qt::Key_Return || event->key() == Qt::Key_Enter)
        && !path.isEmpty()
        && !current.data(FamListModel::DirectoryRole).toBool()) {
        emit openRequested(path);
        return;
    }
    QListView::keyPressEvent(event);
}

void FileView::mouseDoubleClickEvent(QMouseEvent *event) {
    /* The base class routes this to the edit machinery the rows do not have, and a folder is
     * already open by the click that selected it; the only new fact a double-click carries is
     * "open this file". */
    const QModelIndex index = indexAt(event->position().toPoint());
    if (index.isValid() && !index.data(FamListModel::DirectoryRole).toBool()) {
        const QString path = index.data(FamListModel::PathRole).toString();
        if (!path.isEmpty()) {
            emit openRequested(path);
            return;
        }
    }
    QListView::mouseDoubleClickEvent(event);
}

void FileView::contextMenuEvent(QContextMenuEvent *event) {
    const QModelIndex index = indexAt(event->pos());
    if (!index.isValid()) {
        event->ignore();
        return;
    }
    const bool isDirectory = index.data(FamListModel::DirectoryRole).toBool();
    const QString path = index.data(FamListModel::PathRole).toString();
    if (path.isEmpty()) {
        event->ignore();
        return;
    }
    QMenu menu(this);
    menu.setObjectName(QString::fromLatin1(Theme::tagChipMenuName()));
    if (isDirectory) {
        /* The one way to descend: right-click → "open in column". A click only previews. */
        connect(menu.addAction(QStringLiteral("Open in column")), &QAction::triggered, this,
                [this, path] { emit openInColumnRequested(path); });
        menu.addSeparator();
    }
    connect(menu.addAction(QStringLiteral("Open")), &QAction::triggered, this,
            [this, path] { emit openRequested(path); });
    connect(menu.addAction(QStringLiteral("Rename")), &QAction::triggered, this,
            [this, path] { emit renameRequested(path); });
    connect(menu.addAction(QStringLiteral("Delete")), &QAction::triggered, this,
            [this, path] { emit deleteRequested(path); });
    menu.exec(event->globalPos());
}

void FileView::dropEvent(QDropEvent *event) {
    /* The drag is over the moment something lands, accepted or not: the border says "would take it",
     * and nothing is being taken any more. */
    setDropHover(false);
    const QStringList paths = pathsOf(event->mimeData());
    if (m_folder.isEmpty() || paths.isEmpty()) {
        event->ignore();
        return;
    }
    /* Off the event rather than off `QApplication::keyboardModifiers()`, because the event's copy is
     * the key state the platform reported at the moment of the drop - and because a headless run can
     * put the same state into an event on purpose instead of having to press something. */
    const bool copy = event->modifiers().testFlag(Qt::ControlModifier);
    event->acceptProposedAction();
    emit dropped(m_folder, paths, copy);
}
