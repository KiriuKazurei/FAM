#include "fileview.h"

#include "famlistmodel.h"
#include "rowdelegate.h"

#include <QDragEnterEvent>
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
    if (takes(event->mimeData())) {
        event->acceptProposedAction();
    } else {
        event->ignore();
    }
}

void FileView::dragMoveEvent(QDragMoveEvent *event) {
    /* Answered again on every move rather than once at the border: a drag manager that gets no answer
     * mid-drag treats the cursor as being over a target that changed its mind, and shows the no-drop
     * cursor. `QDragMoveEvent` is a sibling of `QDragEnterEvent` rather than a derived class, so the
     * two answers are written out twice. */
    if (takes(event->mimeData())) {
        event->acceptProposedAction();
    } else {
        event->ignore();
    }
}

void FileView::dropEvent(QDropEvent *event) {
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
