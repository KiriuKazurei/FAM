#include "pane.h"

#include <QEvent>
#include <QHBoxLayout>
#include <QListView>
#include <QStackedWidget>

#include "iconcache.h"
#include "rowdelegate.h"
#include "theme.h"

Pane::Pane(FamBridge *bridge, IconCache *icons, const QStringList &kinds, QWidget *parent)
    : QWidget(parent) {
    chain = new ColumnChain(bridge, icons, kinds, this);

    /* Search results get their own list and model rather than borrowing a column's, so the columns
     * behind them stay open and keep their watch. */
    model = new FamListModel(this);
    model->setIconCache(icons);
    /* The same second seam the columns get: a search's rows are files, and a person filtering or
     * glancing at hits wants to know which of them are already tagged. Its paths come from the rows
     * themselves rather than from one directory's prefix, which is the only difference. */
    model->setTagFetcher([bridge](const QStringList &paths,
                                  QHash<QString, QStringList> *tags, QString *error) {
        return bridge->tagsForPaths(paths, tags, error);
    });
    model->setNoteFetcher([bridge](const QStringList &paths, QSet<QString> *noted,
                                   QString *error) {
        return bridge->notesForPaths(paths, noted, error);
    });

    results = new FileView(this);
    results->setObjectName(QString::fromLatin1(Theme::listName()));
    results->setModel(model);
    results->setUniformItemSizes(true);
    results->setSelectionBehavior(QAbstractItemView::SelectRows);
    results->setResizeMode(QListView::Adjust);
    /* The delegate paints its own ground, so hover only arrives if the view is told to track it -
     * and a row that cannot show its hover state is a row whose selected state is the only feedback
     * about where the cursor is. */
    auto *rowDelegate = new RowDelegate(&Theme::instance(), kFamRowRoles, results);
    results->setItemDelegate(rowDelegate);
    connect(rowDelegate, &RowDelegate::tagClicked, results, &FileView::relayTagClick);
    results->setMouseTracking(true);
    results->viewport()->setAttribute(Qt::WA_Hover, true);
    /* Left a source-only FileView: no folder is named for it, so a drop that lands here is refused at
     * the border rather than guessed at. Its rows can still be dragged out into a column. */

    pages = new QStackedWidget(this);
    pages->addWidget(chain);
    pages->addWidget(results);

    QObject::connect(chain, &ColumnChain::dropped, this, &Pane::dropped);
    QObject::connect(chain, &ColumnChain::tagActivated, this, &Pane::tagActivated);
    QObject::connect(results, &FileView::tagActivated, this, &Pane::tagActivated);

    /* The pane needs a layout of its own. Without one, `pages` is a child that keeps whatever
     * geometry it was born with, and the window can hand this pane half the screen while the listing
     * inside it stays a 340px strip - which is exactly the "single column does not fill the window"
     * bug this pane caused on the way in. */
    auto *shell = new QHBoxLayout(this);
    shell->setContentsMargins(0, 0, 0, 0);
    shell->setSpacing(0);
    shell->addWidget(pages);

    watch(this);
    watch(results);
    watch(results->viewport());
}

void Pane::watch(QWidget *widget) {
    if (widget != nullptr) {
        widget->installEventFilter(this);
    }
}

bool Pane::eventFilter(QObject *watched, QEvent *event) {
    switch (event->type()) {
    case QEvent::MouseButtonPress:
    case QEvent::FocusIn:
        emit activated();
        break;
    default:
        Q_UNUSED(watched);
        break;
    }
    /* A filter that consumes nothing: the view still gets the click that moves the cursor. */
    return false;
}

void Pane::showResults() {
    m_resultsPage = 1;
    pages->setCurrentIndex(m_resultsPage);
}

void Pane::showFolder() {
    m_resultsPage = 0;
    pages->setCurrentIndex(m_resultsPage);
}
